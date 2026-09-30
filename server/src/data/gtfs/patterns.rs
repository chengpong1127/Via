use std::collections::{BTreeMap, HashMap};

use gtfs_structures::{
    DirectionType, ExactTimes, RawFrequency, RawStopTime, RawTrip, Route, Shape,
};

use crate::domain::{
    ids::{ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId},
    transit::{
        ScheduledStopTime, SegmentTimeSource, ServiceCalendar, TransitFrequency, TransitMode,
        TransitPattern, TransitStop, TransitTrip,
    },
};

use super::{
    bindings,
    normalize::{has_unique_stops, is_chronological_times, valid_coordinate},
    segment_profile,
};

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

pub(super) struct GtfsIndexes<'a> {
    pub(super) stops: &'a [TransitStop],
    pub(super) stop_index_by_gtfs_id: &'a HashMap<String, u32>,
    pub(super) routes: &'a [Route],
    pub(super) route_index_by_id: &'a HashMap<String, usize>,
    pub(super) route_modes: &'a HashMap<String, TransitMode>,
}

pub(super) struct PatternTimingCounts {
    pub(super) exact_static: usize,
    pub(super) derived_bus: usize,
    pub(super) geometric_fallback: usize,
}

pub(super) struct PatternNormalization {
    pub(super) patterns: Vec<TransitPattern>,
    pub(super) scheduled_trips: Vec<TransitTrip>,
    pub(super) frequencies: Vec<TransitFrequency>,
    pub(super) timing_counts: PatternTimingCounts,
    pub(super) invalid_trip_count: usize,
    pub(super) invalid_trip_examples: Vec<String>,
}

pub(super) fn normalize(
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
