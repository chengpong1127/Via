use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

use gtfs_structures::{
    Calendar, CalendarDate, DirectionType, ExactTimes, Exception, ExtendedRouteType, LocationType,
    RawFrequency, RawGtfs, RawStopTime, RawTrip, Route, RouteType, Shape,
};
use time::{Date, Month, Weekday};

use crate::{
    data::DataError,
    domain::{
        ids::{
            BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
        },
        location::RequestCoordinate,
        transit::{
            ScheduledStopTime, SegmentTimeSource, ServiceCalendar, TransitFrequency, TransitMode,
            TransitNetwork, TransitPattern, TransitRoute, TransitStop, TransitTrip,
            default_service_day_boundary,
        },
    },
};

use super::{
    GtfsBuildDiagnostics,
    bindings::{self, BindingInputs},
    segment_profile,
};

pub(super) struct NormalizationOutput {
    pub(super) network: TransitNetwork,
    pub(super) binding_inputs: BindingInputs,
}

struct ViaStops {
    stops: Vec<TransitStop>,
    index_by_gtfs_id: HashMap<String, u32>,
    valid_gtfs_stop_ids: Vec<String>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ServicePatternKey {
    route_id: String,
    direction: Option<u8>,
    stops: Vec<u32>,
}

#[derive(Clone, Debug)]
struct ServiceBuilder {
    route_index: usize,
    route_id: String,
    direction: Option<u8>,
    stops: Vec<u32>,
    headsigns: BTreeMap<String, usize>,
    scheduled_trips: Vec<TransitTrip>,
    frequencies: Vec<TransitFrequency>,
    bus_segment_samples: Vec<Vec<u32>>,
}

impl ServiceBuilder {
    fn new(key: &ServicePatternKey, route_index: usize) -> Self {
        Self {
            route_index,
            route_id: key.route_id.clone(),
            direction: key.direction,
            stops: key.stops.clone(),
            headsigns: BTreeMap::new(),
            scheduled_trips: Vec::new(),
            frequencies: Vec::new(),
            bus_segment_samples: vec![Vec::new(); key.stops.len().saturating_sub(1)],
        }
    }

    fn add_headsign(&mut self, value: Option<&str>) {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            *self.headsigns.entry(value.to_owned()).or_default() += 1;
        }
    }

    fn headsign(&self, route: &Route) -> String {
        self.headsigns
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
            .map(|(value, _)| value.clone())
            .unwrap_or_else(|| route_headsign_fallback(route))
    }
}

pub(super) fn route_headsign_fallback(route: &Route) -> String {
    route
        .long_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            route
                .short_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or(&route.id)
        .to_owned()
}

struct GtfsIndexes<'a> {
    stops: &'a [TransitStop],
    stop_index_by_gtfs_id: &'a HashMap<String, u32>,
    routes: &'a [Route],
    route_index_by_id: &'a HashMap<String, usize>,
    route_modes: &'a HashMap<String, TransitMode>,
}

struct PatternTimingCounts {
    exact_static: usize,
    derived_bus: usize,
    geometric_fallback: usize,
}

struct PatternNormalization {
    patterns: Vec<TransitPattern>,
    scheduled_trips: Vec<TransitTrip>,
    frequencies: Vec<TransitFrequency>,
    timing_counts: PatternTimingCounts,
    invalid_trip_count: usize,
    invalid_trip_examples: Vec<String>,
}

pub(super) fn normalize(
    raw: RawGtfs,
    input: &Path,
    diagnostics: &mut GtfsBuildDiagnostics,
) -> Result<NormalizationOutput, DataError> {
    let raw_stops = raw
        .stops
        .map_err(|error| super::invalid_gtfs(input, format!("stops.txt: {error}")))?;
    let raw_routes = raw
        .routes
        .map_err(|error| super::invalid_gtfs(input, format!("routes.txt: {error}")))?;
    let raw_trips = raw
        .trips
        .map_err(|error| super::invalid_gtfs(input, format!("trips.txt: {error}")))?;
    let stop_times = raw
        .stop_times
        .map_err(|error| super::invalid_gtfs(input, format!("stop_times.txt: {error}")))?;
    let calendars = optional_table(raw.calendar, input, "calendar.txt")?;
    let calendar_dates = optional_table(raw.calendar_dates, input, "calendar_dates.txt")?;
    let frequencies = optional_table(raw.frequencies, input, "frequencies.txt")?;
    let shapes = optional_table(raw.shapes, input, "shapes.txt")?;

    let calendars = via_calendars(calendars, calendar_dates);
    let ViaStops {
        stops,
        index_by_gtfs_id: stop_index_by_gtfs_id,
        valid_gtfs_stop_ids,
    } = via_stops(raw_stops)?;
    let route_modes = route_modes(&raw_routes, diagnostics);
    let route_index_by_id = raw_routes
        .iter()
        .enumerate()
        .map(|(index, route)| (route.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut usable_trip_ids = HashSet::new();
    let mut usable_trips = Vec::new();
    for trip in &raw_trips {
        if route_modes.contains_key(trip.route_id.as_str())
            && usable_trip_ids.insert(trip.id.as_str())
        {
            usable_trips.push(trip);
        }
    }
    diagnostics.trips_loaded = usable_trips.len();

    let used_shape_ids = usable_trips
        .iter()
        .filter_map(|trip| trip.shape_id.as_deref())
        .collect::<HashSet<_>>();
    diagnostics.shape_rows_loaded = shapes
        .iter()
        .filter(|shape| {
            used_shape_ids.contains(shape.id.as_str())
                && valid_coordinate(shape.latitude, shape.longitude)
        })
        .count();

    let indexes = GtfsIndexes {
        stops: &stops,
        stop_index_by_gtfs_id: &stop_index_by_gtfs_id,
        routes: &raw_routes,
        route_index_by_id: &route_index_by_id,
        route_modes: &route_modes,
    };
    let mut stop_times_by_trip = HashMap::<&str, Vec<&RawStopTime>>::new();
    for stop_time in &stop_times {
        if usable_trip_ids.contains(stop_time.trip_id.as_str())
            && stop_index_by_gtfs_id.contains_key(stop_time.stop_id.as_str())
        {
            stop_times_by_trip
                .entry(&stop_time.trip_id)
                .or_default()
                .push(stop_time);
        }
    }
    for times in stop_times_by_trip.values_mut() {
        times.sort_by_key(|stop_time| stop_time.stop_sequence);
    }
    let mut frequencies_by_trip = HashMap::<String, Vec<_>>::new();
    for frequency in frequencies {
        frequencies_by_trip
            .entry(frequency.trip_id.clone())
            .or_default()
            .push(frequency);
    }

    let normalized = normalize_patterns(
        &indexes,
        &usable_trips,
        &stop_times_by_trip,
        &frequencies_by_trip,
        &calendars,
        &shapes,
    );
    diagnostics.invalid_timing_trips = normalized.invalid_trip_count;
    diagnostics.invalid_timing_trip_examples = normalized.invalid_trip_examples;
    diagnostics.scheduled_trips_loaded = normalized.scheduled_trips.len();
    diagnostics.frequencies_loaded = normalized.frequencies.len();
    diagnostics.exact_static_timing_patterns = normalized.timing_counts.exact_static;
    diagnostics.derived_bus_timing_patterns = normalized.timing_counts.derived_bus;
    diagnostics.geometric_fallback_patterns = normalized.timing_counts.geometric_fallback;
    let mut patterns = normalized.patterns;
    let scheduled_trips = normalized.scheduled_trips;
    let mut normalized_frequencies = normalized.frequencies;

    let route_by_via_id = raw_routes
        .iter()
        .filter_map(|route| {
            route_modes
                .get(route.id.as_str())
                .map(|mode| (bindings::via_route_id(route, *mode), route))
        })
        .collect::<BTreeMap<_, _>>();

    for pattern in &patterns {
        let route = route_by_via_id[pattern.route_id.as_str()];
        let mode = via_mode(route.route_type, &route.id).expect("normalized route is supported");
        *diagnostics
            .patterns_loaded_by_mode
            .entry(mode_name(mode).to_owned())
            .or_default() += 1;
    }
    let used_stop_ids = patterns
        .iter()
        .flat_map(|pattern| pattern.stops.iter().cloned())
        .collect::<HashSet<_>>();
    for (stop, gtfs_stop_id) in stops.iter().zip(&valid_gtfs_stop_ids) {
        if !used_stop_ids.contains(&stop.id) {
            diagnostics.valid_unreferenced_stops += 1;
            super::push_example(&mut diagnostics.unreferenced_stop_examples, gtfs_stop_id);
        }
    }
    let stops = stops
        .into_iter()
        .filter(|stop| used_stop_ids.contains(&stop.id))
        .collect::<Vec<_>>();
    diagnostics.stops_loaded = stops.len();
    let used_calendars = scheduled_trips
        .iter()
        .map(|trip| trip.calendar_id.as_str())
        .chain(
            normalized_frequencies
                .iter()
                .map(|frequency| frequency.calendar_id.as_str()),
        )
        .collect::<HashSet<_>>();
    let calendars = calendars
        .into_values()
        .filter(|calendar| used_calendars.contains(calendar.id.as_str()))
        .collect();
    normalized_frequencies.shrink_to_fit();
    patterns.shrink_to_fit();
    let routes = route_by_via_id
        .into_iter()
        .map(|(id, route)| {
            let mode = route_modes[route.id.as_str()];
            let label = match mode {
                TransitMode::Bus => route
                    .short_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(&route.id)
                    .to_owned(),
                TransitMode::Metro => id.strip_prefix("metro:").unwrap_or(&id).to_owned(),
                TransitMode::Rail => "TRA".to_owned(),
                TransitMode::HighSpeedRail => "THSR".to_owned(),
            };
            TransitRoute {
                id: TransitRouteId::new(id),
                mode,
                label,
            }
        })
        .collect();
    let network = TransitNetwork::new(
        default_service_day_boundary(),
        stops,
        patterns,
        scheduled_trips,
    )
    .with_calendars(calendars)
    .with_routes(routes)
    .with_frequencies(normalized_frequencies);

    Ok(NormalizationOutput {
        network,
        binding_inputs: BindingInputs {
            routes: raw_routes,
            route_modes,
            trips: raw_trips,
        },
    })
}

fn normalize_patterns(
    indexes: &GtfsIndexes<'_>,
    trips: &[&RawTrip],
    stop_times_by_trip: &HashMap<&str, Vec<&RawStopTime>>,
    frequencies_by_trip: &HashMap<String, Vec<RawFrequency>>,
    calendars: &HashMap<String, ServiceCalendar>,
    shapes: &[Shape],
) -> PatternNormalization {
    let mut shapes_by_id = HashMap::<&str, Vec<&Shape>>::new();
    for shape in shapes {
        if valid_coordinate(shape.latitude, shape.longitude) {
            shapes_by_id.entry(&shape.id).or_default().push(shape);
        }
    }

    let mut builders = Vec::<ServiceBuilder>::new();
    let mut service_indices = HashMap::<ServicePatternKey, usize>::new();
    let mut invalid_trip_count = 0;
    let mut invalid_trip_examples = Vec::new();
    for trip in trips {
        let Some(route_index) = indexes
            .route_index_by_id
            .get(trip.route_id.as_str())
            .copied()
        else {
            continue;
        };
        let Some(rows) = stop_times_by_trip.get(trip.id.as_str()) else {
            continue;
        };
        let pattern = rows
            .iter()
            .filter_map(|row| {
                indexes
                    .stop_index_by_gtfs_id
                    .get(row.stop_id.as_str())
                    .copied()
            })
            .collect::<Vec<_>>();
        if pattern.len() < 2 || !has_unique_stops(&pattern) {
            continue;
        }

        let route_id = bindings::via_route_id(
            &indexes.routes[route_index],
            indexes.route_modes[trip.route_id.as_str()],
        );
        let direction = trip
            .direction_id
            .map(|direction| match direction {
                DirectionType::Outbound => 0,
                DirectionType::Inbound => 1,
            })
            .or_else(|| bindings::direction_from_route_id(&trip.route_id));
        let key = ServicePatternKey {
            route_id,
            direction,
            stops: pattern.clone(),
        };
        let service_index = *service_indices.entry(key.clone()).or_insert_with(|| {
            builders.push(ServiceBuilder::new(&key, route_index));
            builders.len() - 1
        });
        let builder = &mut builders[service_index];
        builder.add_headsign(trip.trip_headsign.as_deref());

        let template = rows
            .iter()
            .map(|row| {
                ScheduledStopTime::new(
                    row.arrival_time.unwrap_or_default(),
                    row.departure_time.unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let complete_times = rows
            .iter()
            .all(|row| row.arrival_time.is_some() && row.departure_time.is_some());
        let chronological = complete_times && is_chronological_times(&template);
        if complete_times && !chronological {
            invalid_trip_count += 1;
            if invalid_trip_examples.len() < 20 {
                invalid_trip_examples.push(trip.id.clone());
            }
        }

        let calendar = calendars.get(&trip.service_id);
        let has_frequencies = frequencies_by_trip
            .get(&trip.id)
            .is_some_and(|rows| rows.iter().any(valid_frequency));
        if chronological && !has_frequencies && calendar.is_some_and(calendar_is_usable) {
            builder.scheduled_trips.push(TransitTrip::new(
                TransitTripId::new(trip.id.clone()),
                TransitPatternId::new("pending"),
                ServiceCalendarId::new(trip.service_id.clone()),
                template.clone(),
            ));
        }
        if chronological && calendar.is_some_and(calendar_is_usable) {
            if let Some(frequency_rows) = frequencies_by_trip.get(&trip.id) {
                for (frequency_index, frequency) in frequency_rows
                    .iter()
                    .filter(|frequency| valid_frequency(frequency))
                    .enumerate()
                {
                    builder.frequencies.push(TransitFrequency {
                        id: TransitTripId::new(format!("{}::frequency={frequency_index}", trip.id)),
                        pattern_id: TransitPatternId::new("pending"),
                        calendar_id: ServiceCalendarId::new(trip.service_id.clone()),
                        start_seconds: frequency.start_time,
                        end_seconds: frequency.end_time,
                        headway_seconds: frequency.headway_secs,
                        exact_times: frequency.exact_times == Some(ExactTimes::ScheduleBased),
                        template_stop_times: template.clone(),
                    });
                }
            }
        }

        if indexes.route_modes[trip.route_id.as_str()] == TransitMode::Bus
            && let (Some(first_departure), Some(last_arrival)) = (
                rows.first().and_then(|row| row.departure_time),
                rows.last().and_then(|row| row.arrival_time),
            )
            && last_arrival > first_departure
        {
            let distances = segment_profile::trip_segment_distances(
                indexes.stops,
                indexes.stop_index_by_gtfs_id,
                rows,
                trip,
                &shapes_by_id,
            );
            if let Some(samples) =
                segment_profile::distribute_runtime(last_arrival - first_departure, &distances)
            {
                for (segment, seconds) in builder.bus_segment_samples.iter_mut().zip(samples) {
                    segment.push(seconds);
                }
            }
        }
    }

    let mut groups = BTreeMap::<(String, Option<u8>), Vec<usize>>::new();
    for (index, builder) in builders.iter().enumerate() {
        groups
            .entry((builder.route_id.clone(), builder.direction))
            .or_default()
            .push(index);
    }
    let mut pattern_ids = vec![TransitPatternId::new(""); builders.len()];
    let mut patterns = Vec::with_capacity(builders.len());
    for ((route_id, direction), mut indices) in groups {
        indices.sort_by(|left, right| builders[*left].stops.cmp(&builders[*right].stops));
        for (position, builder_index) in indices.iter().enumerate() {
            let builder = &builders[*builder_index];
            let id = TransitPatternId::new(format!(
                "pattern:{route_id}::direction={}::variant={position}",
                direction.map_or("unknown".to_owned(), |value| value.to_string())
            ));
            pattern_ids[*builder_index] = id.clone();
            let route = &indexes.routes[builder.route_index];
            patterns.push(TransitPattern::new(
                id,
                TransitRouteId::new(route_id.clone()),
                direction,
                builder.headsign(route),
                builder
                    .stops
                    .iter()
                    .map(|index| indexes.stops[*index as usize].id.clone())
                    .collect(),
                segment_profile::segment_profile(
                    &builder.stops,
                    &builder.scheduled_trips,
                    &builder.frequencies,
                    &builder.bus_segment_samples,
                    indexes.route_modes[route.id.as_str()],
                    indexes.stops,
                ),
            ));
        }
    }

    let exact_count = patterns
        .iter()
        .filter(|pattern| {
            pattern
                .static_profile
                .segments
                .iter()
                .all(|segment| segment.source == SegmentTimeSource::Schedule)
        })
        .count();
    let derived_count = patterns
        .iter()
        .filter(|pattern| {
            pattern
                .static_profile
                .segments
                .iter()
                .all(|segment| segment.source == SegmentTimeSource::Derived)
        })
        .count();
    let geometric_count = patterns.len() - exact_count - derived_count;
    let mut scheduled_trips = Vec::new();
    let mut normalized_frequencies = Vec::new();
    for (index, builder) in builders.into_iter().enumerate() {
        let pattern_id = pattern_ids[index].clone();
        for mut trip in builder.scheduled_trips {
            trip.pattern_id = pattern_id.clone();
            scheduled_trips.push(trip);
        }
        for mut frequency in builder.frequencies {
            frequency.pattern_id = pattern_id.clone();
            normalized_frequencies.push(frequency);
        }
    }

    PatternNormalization {
        patterns,
        scheduled_trips,
        frequencies: normalized_frequencies,
        timing_counts: PatternTimingCounts {
            exact_static: exact_count,
            derived_bus: derived_count,
            geometric_fallback: geometric_count,
        },
        invalid_trip_count,
        invalid_trip_examples,
    }
}

fn valid_frequency(frequency: &RawFrequency) -> bool {
    frequency.headway_secs > 0 && frequency.start_time <= frequency.end_time
}

fn calendar_is_usable(calendar: &ServiceCalendar) -> bool {
    !calendar.weekdays.is_empty() || !calendar.added_dates.is_empty()
}

pub(super) fn is_chronological_times(times: &[ScheduledStopTime]) -> bool {
    !times.is_empty()
        && times
            .iter()
            .all(|time| time.arrival_seconds <= time.departure_seconds)
        && times
            .windows(2)
            .all(|window| window[0].departure_seconds <= window[1].arrival_seconds)
}

pub(super) fn has_unique_stops(stops: &[u32]) -> bool {
    stops.iter().copied().collect::<HashSet<_>>().len() == stops.len()
}

fn optional_table<T>(
    table: Option<Result<Vec<T>, gtfs_structures::Error>>,
    input: &Path,
    name: &str,
) -> Result<Vec<T>, DataError> {
    table
        .unwrap_or(Ok(Vec::new()))
        .map_err(|error| super::invalid_gtfs(input, format!("{name}: {error}")))
}

fn via_stops(source: Vec<gtfs_structures::Stop>) -> Result<ViaStops, DataError> {
    let mut stops = Vec::new();
    let mut index_by_gtfs_id = HashMap::new();
    let mut valid_gtfs_stop_ids = Vec::new();
    for stop in source {
        if stop.location_type != LocationType::StopPoint {
            continue;
        }
        let id = stop.id.trim();
        let Some((latitude, longitude)) = stop.latitude.zip(stop.longitude) else {
            continue;
        };
        if id.is_empty()
            || !valid_coordinate(latitude, longitude)
            || index_by_gtfs_id.contains_key(id)
        {
            continue;
        }
        let index = stops.len() as u32;
        index_by_gtfs_id.insert(id.to_owned(), index);
        valid_gtfs_stop_ids.push(id.to_owned());
        stops.push(TransitStop::new(
            BoardingPointId::new(bindings::tdx_stop_id(id)),
            stop.name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(id),
            RequestCoordinate {
                latitude,
                longitude,
            },
        ));
    }
    if stops.is_empty() {
        return Err(DataError::EmptyNetwork);
    }
    Ok(ViaStops {
        stops,
        index_by_gtfs_id,
        valid_gtfs_stop_ids,
    })
}

fn valid_coordinate(latitude: f64, longitude: f64) -> bool {
    RequestCoordinate {
        latitude,
        longitude,
    }
    .validate()
}

fn via_calendars(
    source: Vec<Calendar>,
    exceptions: Vec<CalendarDate>,
) -> HashMap<String, ServiceCalendar> {
    let mut calendars = source
        .into_iter()
        .map(|calendar| {
            let weekdays = [
                (calendar.monday, Weekday::Monday),
                (calendar.tuesday, Weekday::Tuesday),
                (calendar.wednesday, Weekday::Wednesday),
                (calendar.thursday, Weekday::Thursday),
                (calendar.friday, Weekday::Friday),
                (calendar.saturday, Weekday::Saturday),
                (calendar.sunday, Weekday::Sunday),
            ]
            .into_iter()
            .filter_map(|(active, weekday)| active.then_some(weekday))
            .collect();
            let id = calendar.id;
            (
                id.clone(),
                ServiceCalendar {
                    id: ServiceCalendarId::new(id),
                    weekdays,
                    start_date: Some(via_date(calendar.start_date)),
                    end_date: Some(via_date(calendar.end_date)),
                    added_dates: Vec::new(),
                    removed_dates: Vec::new(),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    for exception in exceptions {
        let calendar = calendars
            .entry(exception.service_id.clone())
            .or_insert_with(|| empty_calendar(&exception.service_id));
        let dates = match exception.exception_type {
            Exception::Added => &mut calendar.added_dates,
            Exception::Deleted => &mut calendar.removed_dates,
        };
        dates.push(via_date(exception.date));
    }
    for calendar in calendars.values_mut() {
        calendar.added_dates.sort_unstable();
        calendar.added_dates.dedup();
        calendar.removed_dates.sort_unstable();
        calendar.removed_dates.dedup();
    }
    calendars
}

fn empty_calendar(service_id: &str) -> ServiceCalendar {
    ServiceCalendar {
        id: ServiceCalendarId::new(service_id),
        weekdays: Vec::new(),
        start_date: None,
        end_date: None,
        added_dates: Vec::new(),
        removed_dates: Vec::new(),
    }
}

fn via_date(date: jiff::civil::Date) -> Date {
    Date::from_calendar_date(
        i32::from(date.year()),
        Month::try_from(date.month() as u8).expect("GTFS date has a valid month"),
        date.day() as u8,
    )
    .expect("GTFS date is within the supported calendar range")
}

fn route_modes(
    routes: &[Route],
    diagnostics: &mut GtfsBuildDiagnostics,
) -> HashMap<String, TransitMode> {
    let mut modes = HashMap::new();
    for route in routes {
        if let Some(mode) = via_mode(route.route_type, &route.id) {
            modes.insert(route.id.clone(), mode);
            *diagnostics
                .routes_loaded_by_mode
                .entry(mode_name(mode).to_owned())
                .or_default() += 1;
        }
    }
    modes
}

pub(super) fn via_mode(route_type: RouteType, route_id: &str) -> Option<TransitMode> {
    match route_type {
        RouteType::Bus => Some(TransitMode::Bus),
        RouteType::Subway => Some(TransitMode::Metro),
        RouteType::Rail if route_id.starts_with("THSR") => Some(TransitMode::HighSpeedRail),
        RouteType::Rail => Some(TransitMode::Rail),
        RouteType::Extended(ExtendedRouteType::HighSpeedRail) => Some(TransitMode::HighSpeedRail),
        _ => None,
    }
}

fn mode_name(mode: TransitMode) -> &'static str {
    match mode {
        TransitMode::Bus => "bus",
        TransitMode::Metro => "metro",
        TransitMode::Rail => "rail",
        TransitMode::HighSpeedRail => "high_speed_rail",
    }
}
