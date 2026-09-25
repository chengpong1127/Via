//! Read the local GTFS feed and translate its transit data into Via types.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

use gtfs_structures::{
    Calendar, CalendarDate, DirectionType, ExactTimes, Exception, ExtendedRouteType, LocationType,
    RawFrequency, RawGtfs, RawStopTime, RawTrip, Route, RouteType, Shape,
};
use serde::Serialize;
use time::{Date, Month, Weekday};

use crate::{
    data::DataError,
    domain::{
        ids::{
            BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
        },
        location::RequestCoordinate,
        transit::{
            ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime,
            ServiceCalendar, TransitFrequency, TransitMode, TransitNetwork, TransitPattern,
            TransitRoute, TransitStop, TransitTrip, default_service_day_boundary,
        },
    },
    providers::bindings::{
        BusPatternBinding, MetroPatternBinding, TdxProviderBindings, TraTripBinding,
    },
    tdx::transit_service_id_for_direction,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GtfsBuildDiagnostics {
    pub stops_loaded: usize,
    pub routes_loaded_by_mode: BTreeMap<String, usize>,
    pub patterns_loaded_by_mode: BTreeMap<String, usize>,
    pub trips_loaded: usize,
    pub scheduled_trips_loaded: usize,
    pub frequencies_loaded: usize,
    pub shape_rows_loaded: usize,
    pub invalid_timing_trips: usize,
    pub invalid_timing_trip_examples: Vec<String>,
    pub exact_static_timing_patterns: usize,
    pub derived_bus_timing_patterns: usize,
    pub geometric_fallback_patterns: usize,
    pub unresolved_bus_realtime_scope_patterns: usize,
    pub unresolved_bus_realtime_scope_examples: Vec<String>,
    pub gtfs_stop_ids_without_tdx_mapping: usize,
    pub unmapped_stop_id_examples: Vec<String>,
    pub gtfs_pattern_ids_without_tdx_mapping: usize,
    pub unmapped_pattern_id_examples: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct GtfsBuildOutput {
    pub build: super::BuildOutput,
    pub diagnostics: GtfsBuildDiagnostics,
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

fn route_headsign_fallback(route: &Route) -> String {
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
    stop_position_by_id: &'a HashMap<BoardingPointId, u32>,
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

/// Parse GTFS records and produce only Via-owned domain values.
pub(super) fn build_network(
    input: &Path,
) -> Result<(TransitNetwork, TdxProviderBindings, GtfsBuildDiagnostics), DataError> {
    let raw = RawGtfs::from_path(input).map_err(|error| invalid_gtfs(input, error))?;
    let mut diagnostics = GtfsBuildDiagnostics::default();
    let raw_stops = raw
        .stops
        .map_err(|error| invalid_gtfs(input, format!("stops.txt: {error}")))?;
    let raw_routes = raw
        .routes
        .map_err(|error| invalid_gtfs(input, format!("routes.txt: {error}")))?;
    let raw_trips = raw
        .trips
        .map_err(|error| invalid_gtfs(input, format!("trips.txt: {error}")))?;
    let stop_times = raw
        .stop_times
        .map_err(|error| invalid_gtfs(input, format!("stop_times.txt: {error}")))?;
    let calendars = optional_table(raw.calendar, input, "calendar.txt")?;
    let calendar_dates = optional_table(raw.calendar_dates, input, "calendar_dates.txt")?;
    let frequencies = optional_table(raw.frequencies, input, "frequencies.txt")?;
    let shapes = optional_table(raw.shapes, input, "shapes.txt")?;

    let calendars = via_calendars(calendars, calendar_dates);
    let (stops, stop_index_by_gtfs_id) = via_stops(raw_stops)?;
    let route_modes = route_modes(&raw_routes, &mut diagnostics);
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

    let stop_position_by_id = stops
        .iter()
        .enumerate()
        .map(|(index, stop)| (stop.id.clone(), index as u32))
        .collect::<HashMap<_, _>>();
    let indexes = GtfsIndexes {
        stops: &stops,
        stop_index_by_gtfs_id: &stop_index_by_gtfs_id,
        stop_position_by_id: &stop_position_by_id,
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
                .map(|mode| (via_route_id(route, *mode), route))
        })
        .collect::<BTreeMap<_, _>>();
    let provider_bindings = provider_bindings(&indexes, &patterns, &usable_trips, &scheduled_trips);

    for pattern in &patterns {
        let route = route_by_via_id[pattern.route_id.as_str()];
        let mode = via_mode(route.route_type, &route.id).expect("normalized route is supported");
        *diagnostics
            .patterns_loaded_by_mode
            .entry(mode_name(mode).to_owned())
            .or_default() += 1;
    }
    let unresolved_bus_scopes = patterns
        .iter()
        .filter(|pattern| {
            let route = route_by_via_id[pattern.route_id.as_str()];
            route_modes[route.id.as_str()] == TransitMode::Bus
                && bus_realtime_scope(
                    route,
                    &pattern
                        .stops
                        .iter()
                        .filter_map(|id| stop_position_by_id.get(id).copied())
                        .collect::<Vec<_>>(),
                    &stops,
                )
                .is_none()
        })
        .map(|pattern| pattern.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.unresolved_bus_realtime_scope_patterns = unresolved_bus_scopes.len();
    diagnostics.unresolved_bus_realtime_scope_examples =
        unresolved_bus_scopes.into_iter().take(20).collect();

    let used_stop_ids = patterns
        .iter()
        .flat_map(|pattern| pattern.stops.iter().cloned())
        .collect::<HashSet<_>>();
    let stops = stops
        .into_iter()
        .filter(|stop| used_stop_ids.contains(&stop.id))
        .collect::<Vec<_>>();
    diagnostics.stops_loaded = stops.len();
    let unmapped_stops = stops
        .iter()
        .filter(|stop| !is_tdx_stop_id(stop.id.as_str()))
        .map(|stop| stop.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.gtfs_stop_ids_without_tdx_mapping = unmapped_stops.len();
    diagnostics.unmapped_stop_id_examples = unmapped_stops.into_iter().take(20).collect();

    let unmapped_patterns = patterns
        .iter()
        .filter(|pattern| {
            !provider_bindings.bus_patterns.contains_key(&pattern.id)
                && !provider_bindings.metro_patterns.contains_key(&pattern.id)
                && !matches!(
                    route_modes.get(route_by_via_id[pattern.route_id.as_str()].id.as_str()),
                    Some(TransitMode::Rail | TransitMode::HighSpeedRail)
                )
        })
        .map(|pattern| pattern.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.gtfs_pattern_ids_without_tdx_mapping = unmapped_patterns.len();
    diagnostics.unmapped_pattern_id_examples = unmapped_patterns.into_iter().take(20).collect();

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
    .with_frequencies(normalized_frequencies)
    .canonicalize();

    Ok((network, provider_bindings, diagnostics))
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

        let route_id = via_route_id(
            &indexes.routes[route_index],
            indexes.route_modes[trip.route_id.as_str()],
        );
        let direction = trip
            .direction_id
            .map(|direction| match direction {
                DirectionType::Outbound => 0,
                DirectionType::Inbound => 1,
            })
            .or_else(|| direction_from_route_id(&trip.route_id));
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
            let distances = trip_segment_distances(
                indexes.stops,
                indexes.stop_index_by_gtfs_id,
                rows,
                trip,
                &shapes_by_id,
            );
            if let Some(samples) = distribute_runtime(last_arrival - first_departure, &distances) {
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
                segment_profile(
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

fn is_chronological_times(times: &[ScheduledStopTime]) -> bool {
    !times.is_empty()
        && times
            .iter()
            .all(|time| time.arrival_seconds <= time.departure_seconds)
        && times
            .windows(2)
            .all(|window| window[0].departure_seconds <= window[1].arrival_seconds)
}

fn has_unique_stops(stops: &[u32]) -> bool {
    stops.iter().copied().collect::<HashSet<_>>().len() == stops.len()
}

fn optional_table<T>(
    table: Option<Result<Vec<T>, gtfs_structures::Error>>,
    input: &Path,
    name: &str,
) -> Result<Vec<T>, DataError> {
    table
        .unwrap_or(Ok(Vec::new()))
        .map_err(|error| invalid_gtfs(input, format!("{name}: {error}")))
}

fn invalid_gtfs(input: &Path, detail: impl std::fmt::Display) -> DataError {
    DataError::InvalidGtfsArchive {
        path: input.into(),
        detail: detail.to_string(),
    }
}

fn via_stops(
    source: Vec<gtfs_structures::Stop>,
) -> Result<(Vec<TransitStop>, HashMap<String, u32>), DataError> {
    let mut stops = Vec::new();
    let mut index_by_gtfs_id = HashMap::new();
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
        stops.push(TransitStop::new(
            BoardingPointId::new(tdx_stop_id(id)),
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
    Ok((stops, index_by_gtfs_id))
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

fn via_mode(route_type: RouteType, route_id: &str) -> Option<TransitMode> {
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

fn via_route_id(route: &Route, mode: TransitMode) -> String {
    match mode {
        TransitMode::Bus => directional_route_suffix(&route.id)
            .map_or_else(|| route.id.clone(), |(base, _)| base.to_owned()),
        TransitMode::Metro => {
            let (line, _, _) = metro_route_parts(&route.id, None);
            format!("metro:{line}")
        }
        TransitMode::Rail => "tra".to_owned(),
        TransitMode::HighSpeedRail => "thsr".to_owned(),
    }
}

fn direction_from_route_id(route_id: &str) -> Option<u8> {
    directional_route_suffix(route_id).map(|(_, direction)| direction)
}

fn directional_route_suffix(route_id: &str) -> Option<(&str, u8)> {
    let (base, suffix) = route_id.rsplit_once('_')?;
    let direction = suffix
        .parse::<u8>()
        .ok()
        .filter(|direction| *direction <= 1)?;
    Some((base, direction))
}

fn metro_route_parts(route_id: &str, direction: Option<u8>) -> (String, u8, String) {
    let (without_operator, operator) = route_id
        .split_once('_')
        .map(|(operator, rest)| (rest, operator))
        .unwrap_or((route_id, ""));
    let mut parts = without_operator.rsplitn(2, '_');
    let suffix = parts.next().unwrap_or_default();
    let prefix = parts.next().unwrap_or(without_operator);
    let (route_direction, subroute) = match suffix.parse::<u8>() {
        Ok(route_direction) => (direction.unwrap_or(route_direction), prefix.to_owned()),
        Err(_) => (direction.unwrap_or(0), without_operator.to_owned()),
    };
    let line = if operator == "TRTC" {
        subroute.split('_').next().unwrap_or(&subroute).to_owned()
    } else {
        format!("{operator}_{subroute}")
    };
    (line, route_direction, subroute)
}

fn provider_bindings(
    indexes: &GtfsIndexes<'_>,
    patterns: &[TransitPattern],
    trips: &[&RawTrip],
    scheduled_trips: &[TransitTrip],
) -> TdxProviderBindings {
    let mut route_by_via_id = HashMap::<String, &Route>::new();
    let mut route_by_via_direction = HashMap::<(String, u8), &Route>::new();
    for route in indexes.routes {
        let Some(mode) = indexes.route_modes.get(&route.id) else {
            continue;
        };
        let via_id = via_route_id(route, *mode);
        route_by_via_id.insert(via_id.clone(), route);
        if let Some(direction) = direction_from_route_id(&route.id) {
            route_by_via_direction.insert((via_id, direction), route);
        }
    }

    let mut bindings = TdxProviderBindings::default();
    for pattern in patterns {
        let route = pattern
            .direction
            .and_then(|direction| {
                route_by_via_direction.get(&(pattern.route_id.as_str().to_owned(), direction))
            })
            .or_else(|| route_by_via_id.get(pattern.route_id.as_str()));
        let Some(route) = route else {
            continue;
        };
        let mode = indexes.route_modes[route.id.as_str()];
        match mode {
            TransitMode::Bus => {
                let positions = pattern
                    .stops
                    .iter()
                    .filter_map(|id| indexes.stop_position_by_id.get(id).copied())
                    .collect::<Vec<_>>();
                if let Some(realtime_scope) = bus_realtime_scope(route, &positions, indexes.stops) {
                    bindings.bus_patterns.insert(
                        pattern.id.clone(),
                        BusPatternBinding {
                            tdx_service_id: canonical_service_id(route, pattern.direction)
                                .as_str()
                                .to_owned(),
                            realtime_scope,
                        },
                    );
                }
            }
            TransitMode::Metro => {
                let (line_id, direction, _) = metro_route_parts(&route.id, pattern.direction);
                bindings.metro_patterns.insert(
                    pattern.id.clone(),
                    MetroPatternBinding { line_id, direction },
                );
            }
            TransitMode::Rail | TransitMode::HighSpeedRail => {}
        }
    }

    let scheduled_ids = scheduled_trips
        .iter()
        .map(|trip| trip.id.as_str())
        .collect::<HashSet<_>>();
    for trip in trips {
        if !scheduled_ids.contains(trip.id.as_str()) {
            continue;
        }
        let Some(route) = indexes
            .route_index_by_id
            .get(trip.route_id.as_str())
            .map(|index| &indexes.routes[*index])
        else {
            continue;
        };
        if indexes.route_modes.get(&route.id) == Some(&TransitMode::Rail)
            && let Some(train_number) = route
                .id
                .strip_prefix("TRA_")
                .and_then(|suffix| suffix.split('_').next())
            && !train_number.is_empty()
        {
            bindings.tra_trips.insert(
                TransitTripId::new(trip.id.clone()),
                TraTripBinding {
                    train_number: train_number.to_owned(),
                },
            );
        }
    }
    bindings
}

fn canonical_service_id(route: &Route, direction: Option<u8>) -> TransitPatternId {
    let mode = via_mode(route.route_type, &route.id);
    if mode == Some(TransitMode::Bus) {
        if let Some(direction) = direction {
            let (base_id, encoded_direction) =
                directional_route_suffix(&route.id).unwrap_or((&route.id, direction));
            let route_id = if encoded_direction == direction {
                base_id
            } else {
                &route.id
            };
            return transit_service_id_for_direction(route_id, direction);
        }
        return TransitPatternId::new(route.id.clone());
    }
    if mode == Some(TransitMode::Metro) {
        let (line, route_direction, subroute) = metro_route_parts(&route.id, direction);
        return TransitPatternId::new(format!(
            "metro:{line}::direction={route_direction}::route={subroute}"
        ));
    }
    if matches!(mode, Some(TransitMode::Rail | TransitMode::HighSpeedRail))
        && let Some(direction) = direction
        && !route.id.ends_with(&format!("_{direction}"))
    {
        return transit_service_id_for_direction(&route.id, direction);
    }
    TransitPatternId::new(route.id.clone())
}

fn bus_realtime_scope(route: &Route, pattern: &[u32], stops: &[TransitStop]) -> Option<String> {
    if via_mode(route.route_type, &route.id) != Some(TransitMode::Bus) {
        return None;
    }
    let scope = city_scope(authority_prefix(&route.id)).or_else(|| {
        route
            .agency_id
            .as_deref()
            .map(authority_prefix)
            .and_then(city_scope)
    });
    scope.map(str::to_owned).or_else(|| {
        (!pattern.is_empty()
            && pattern.iter().all(|index| {
                stops.get(*index as usize).is_some_and(|stop| {
                    (24.8..=25.4).contains(&stop.coordinate.latitude)
                        && (121.2..=122.1).contains(&stop.coordinate.longitude)
                })
            }))
        .then(|| "Taipei".to_owned())
    })
}

fn authority_prefix(value: &str) -> &str {
    let length = value
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .take(3)
        .map(char::len_utf8)
        .sum();
    &value[..length]
}

fn city_scope(route_prefix: &str) -> Option<&'static str> {
    match route_prefix {
        "TPE" => Some("Taipei"),
        "NWT" => Some("NewTaipei"),
        "TAO" => Some("Taoyuan"),
        "HSZ" | "HSQ" => Some("Hsinchu"),
        "MIA" => Some("Miaoli"),
        "TXG" => Some("Taichung"),
        "CHW" => Some("Changhua"),
        "NTO" => Some("Nantou"),
        "YUN" => Some("Yunlin"),
        "CYI" => Some("Chiayi"),
        "CYQ" => Some("ChiayiCounty"),
        "TNN" => Some("Tainan"),
        "KHH" => Some("Kaohsiung"),
        "PIF" => Some("Pingtung"),
        "ILA" => Some("YilanCounty"),
        "HUA" => Some("HualienCounty"),
        "TTT" => Some("TaitungCounty"),
        "KEE" => Some("Keelung"),
        "PEN" => Some("PenghuCounty"),
        "KIN" => Some("KinmenCounty"),
        "LIE" => Some("LienchiangCounty"),
        _ => None,
    }
}

fn is_tdx_stop_id(id: &str) -> bool {
    [
        "THB", "TPE", "NWT", "TAO", "HSZ", "HSQ", "MIA", "TXG", "CHW", "NTO", "YUN", "CYI", "CYQ",
        "TNN", "KHH", "PIF", "ILA", "HUA", "TTT", "KEE", "PEN", "KIN", "LIE", "TRTC-", "TRA-",
        "THSR-", "NTMC-", "TYMC-", "TMRT-", "KRTC-", "KLRT-",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix))
}

fn tdx_stop_id(id: &str) -> String {
    for prefix in [
        "TRTC", "TRA", "THSR", "NTMC", "TYMC", "TMRT", "KRTC", "KLRT",
    ] {
        if let Some(suffix) = id.strip_prefix(&format!("{prefix}_")) {
            return format!("{prefix}-{suffix}");
        }
    }
    id.to_owned()
}

fn segment_profile(
    pattern: &[u32],
    scheduled_trips: &[TransitTrip],
    frequencies: &[TransitFrequency],
    bus_segment_samples: &[Vec<u32>],
    mode: TransitMode,
    stops: &[TransitStop],
) -> SegmentProfile {
    let segment_count = pattern.len().saturating_sub(1);
    let mut minimums = vec![u32::MAX; segment_count];
    for times in scheduled_trips
        .iter()
        .map(|trip| trip.stop_times.as_slice())
        .chain(
            frequencies
                .iter()
                .map(|frequency| frequency.template_stop_times.as_slice()),
        )
    {
        for (minimum, pair) in minimums.iter_mut().zip(times.windows(2)) {
            let duration = pair[1]
                .arrival_seconds
                .saturating_sub(pair[0].departure_seconds);
            if duration > 0 {
                *minimum = (*minimum).min(duration);
            }
        }
    }
    let estimated = estimated_segments(pattern, mode, stops);
    let segments = minimums
        .into_iter()
        .enumerate()
        .map(|(index, scheduled)| {
            if scheduled != u32::MAX {
                SegmentTravelTime {
                    seconds: scheduled,
                    source: SegmentTimeSource::Schedule,
                }
            } else if mode == TransitMode::Bus
                && let Some(derived) = median(bus_segment_samples[index].clone())
                && derived > 0
            {
                SegmentTravelTime {
                    seconds: derived,
                    source: SegmentTimeSource::Derived,
                }
            } else {
                SegmentTravelTime {
                    seconds: estimated[index],
                    source: SegmentTimeSource::Estimated,
                }
            }
        })
        .collect();
    SegmentProfile { segments }
}

fn estimated_segments(pattern: &[u32], mode: TransitMode, stops: &[TransitStop]) -> Vec<u32> {
    let kmh = match mode {
        TransitMode::Bus => 60.0,
        TransitMode::Metro => 100.0,
        TransitMode::Rail => 160.0,
        TransitMode::HighSpeedRail => 350.0,
    };
    pattern
        .windows(2)
        .map(|pair| {
            (stops[pair[0] as usize]
                .coordinate
                .haversine_distance_metres(stops[pair[1] as usize].coordinate)
                / (kmh / 3.6))
                .ceil()
                .max(1.0) as u32
        })
        .collect()
}

fn trip_segment_distances(
    stops: &[TransitStop],
    stop_index_by_gtfs_id: &HashMap<String, u32>,
    rows: &[&RawStopTime],
    trip: &RawTrip,
    shapes_by_id: &HashMap<&str, Vec<&Shape>>,
) -> Vec<f64> {
    let explicit = rows
        .iter()
        .map(|row| row.shape_dist_traveled.map(f64::from))
        .collect::<Option<Vec<_>>>();
    if let Some(values) = explicit {
        let distances = values
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>();
        if valid_distances(&distances) {
            return distances;
        }
    }
    if let Some(shape) = trip.shape_id.as_deref().and_then(|id| shapes_by_id.get(id))
        && let Some(distances) = stop_distances_on_shape(stops, stop_index_by_gtfs_id, rows, shape)
    {
        return distances;
    }
    rows.windows(2)
        .map(|pair| {
            let from = stops[stop_index_by_gtfs_id[pair[0].stop_id.as_str()] as usize].coordinate;
            let to = stops[stop_index_by_gtfs_id[pair[1].stop_id.as_str()] as usize].coordinate;
            from.haversine_distance_metres(to)
        })
        .collect()
}

fn stop_distances_on_shape(
    stops: &[TransitStop],
    stop_index_by_gtfs_id: &HashMap<String, u32>,
    rows: &[&RawStopTime],
    shape: &[&Shape],
) -> Option<Vec<f64>> {
    if shape.len() < 2 {
        return None;
    }
    let mut points = shape.to_vec();
    points.sort_by_key(|point| point.sequence);
    let mut cumulative = 0.0;
    let mut previous = None;
    let mut distances = Vec::with_capacity(points.len());
    for point in &points {
        if let Some(distance) = point.dist_traveled.map(f64::from).filter(|d| d.is_finite()) {
            cumulative = distance;
        } else if let Some((latitude, longitude)) = previous {
            cumulative +=
                geographic_distance_metres(latitude, longitude, point.latitude, point.longitude);
        }
        previous = Some((point.latitude, point.longitude));
        distances.push(cumulative);
    }

    let mut cursor = 0;
    let mut stop_distances = Vec::with_capacity(rows.len());
    for row in rows {
        let stop = stops[stop_index_by_gtfs_id[row.stop_id.as_str()] as usize].coordinate;
        let (index, _) =
            points
                .iter()
                .enumerate()
                .skip(cursor)
                .min_by(|(_, left), (_, right)| {
                    point_distance(stop, left).total_cmp(&point_distance(stop, right))
                })?;
        cursor = index;
        stop_distances.push(distances[index]);
    }
    let segments = stop_distances
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect::<Vec<_>>();
    valid_distances(&segments).then_some(segments)
}

fn point_distance(stop: RequestCoordinate, point: &Shape) -> f64 {
    stop.haversine_distance_metres(RequestCoordinate {
        latitude: point.latitude,
        longitude: point.longitude,
    })
}

fn geographic_distance_metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    RequestCoordinate {
        latitude: lat1,
        longitude: lon1,
    }
    .haversine_distance_metres(RequestCoordinate {
        latitude: lat2,
        longitude: lon2,
    })
}

fn valid_distances(distances: &[f64]) -> bool {
    !distances.is_empty()
        && distances
            .iter()
            .all(|distance| distance.is_finite() && *distance > 0.0)
}

fn distribute_runtime(runtime: u32, distances: &[f64]) -> Option<Vec<u32>> {
    if runtime == 0 || distances.is_empty() || !valid_distances(distances) {
        return None;
    }
    let total_distance: f64 = distances.iter().sum();
    if !total_distance.is_finite() || total_distance <= 0.0 || runtime < distances.len() as u32 {
        return None;
    }
    let mut segments = distances
        .iter()
        .map(|distance| ((f64::from(runtime) * distance / total_distance).floor() as u32).max(1))
        .collect::<Vec<_>>();
    let allocated: u32 = segments.iter().sum();
    if allocated < runtime {
        let mut fractions = distances
            .iter()
            .enumerate()
            .map(|(index, distance)| {
                let exact = f64::from(runtime) * distance / total_distance;
                (index, exact - exact.floor())
            })
            .collect::<Vec<_>>();
        fractions.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
        for (index, _) in fractions.into_iter().take((runtime - allocated) as usize) {
            segments[index] += 1;
        }
    } else if allocated > runtime {
        let mut surplus = allocated - runtime;
        for value in segments.iter_mut().rev() {
            while surplus > 0 && *value > 1 {
                *value -= 1;
                surplus -= 1;
            }
        }
    }
    Some(segments)
}

fn median(mut values: Vec<u32>) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle])
    } else {
        Some(((u64::from(values[middle - 1]) + u64::from(values[middle])) / 2) as u32)
    }
}

#[cfg(test)]
mod tests;
