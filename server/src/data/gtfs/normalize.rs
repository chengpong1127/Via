//! Normalize parsed GTFS records into Via transit domain values.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    domain::{
        ids::{ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId},
        transit::{
            ScheduledStopTime, SegmentTimeSource, ServiceCalendar, TransitFrequency, TransitMode,
            TransitNetwork, TransitPattern, TransitRoute, TransitStop, TransitTrip,
            default_service_day_boundary,
        },
    },
    providers::bindings::TdxProviderBindings,
};

use super::{
    archive::RawGtfsData,
    diagnostics::GtfsBuildDiagnostics,
    identity::{bus_realtime_scope, is_tdx_stop_id, provider_bindings, via_route_id},
    routes::{Route, mode_name},
    schedule::{CompactStopTime, FrequencyRow, GtfsCalendar, TripMeta, calendar_is_usable},
    segment_profile::{distribute_runtime, is_chronological_times, profile_for_builder},
    shapes::{ShapePoint, trip_segment_distances},
};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ServicePatternKey {
    route_id: String,
    direction: Option<u8>,
    pattern: Vec<u32>,
}

#[derive(Clone, Debug)]
struct ServiceBuilder {
    route_index: usize,
    route_id: String,
    direction: Option<u8>,
    pattern: Vec<u32>,
    headsigns: BTreeMap<String, usize>,
    scheduled_trips: Vec<TransitTrip>,
    frequencies: Vec<TransitFrequency>,
    bus_segment_samples: Vec<Vec<u32>>,
}

type NormalizedPatterns = (
    Vec<TransitPattern>,
    Vec<TransitTrip>,
    Vec<TransitFrequency>,
    (usize, usize, usize),
    (usize, Vec<String>),
);

impl ServiceBuilder {
    fn new(key: &ServicePatternKey, route_index: usize) -> Self {
        Self {
            route_index,
            route_id: key.route_id.clone(),
            direction: key.direction,
            pattern: key.pattern.clone(),
            headsigns: BTreeMap::new(),
            scheduled_trips: Vec::new(),
            frequencies: Vec::new(),
            bus_segment_samples: vec![Vec::new(); key.pattern.len().saturating_sub(1)],
        }
    }

    fn add_headsign(&mut self, value: &str) {
        if !value.trim().is_empty() {
            *self.headsigns.entry(value.trim().to_owned()).or_default() += 1;
        }
    }

    fn headsign(&self, route: &Route) -> String {
        self.headsigns
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
            .map(|(value, _)| value.clone())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| route.long_name.clone())
    }
}

pub(super) fn normalize(
    raw: RawGtfsData,
) -> (TransitNetwork, TdxProviderBindings, GtfsBuildDiagnostics) {
    let RawGtfsData {
        stops,
        routes,
        calendars,
        trips,
        shapes,
        frequencies,
        stop_times,
        mut diagnostics,
    } = raw;
    let (mut services, trips_out, mut frequencies_out, timing_classes, invalid_timing_trips) =
        normalize_patterns(
            &stops,
            &routes,
            &trips,
            &calendars,
            &frequencies,
            &stop_times,
            &shapes,
        );
    diagnostics.invalid_timing_trips = invalid_timing_trips.0;
    diagnostics.invalid_timing_trip_examples = invalid_timing_trips.1;
    let route_by_via_id = routes
        .iter()
        .enumerate()
        .map(|(index, route)| (via_route_id(route), index))
        .collect::<HashMap<_, _>>();
    let stop_position_by_id = stops
        .iter()
        .enumerate()
        .map(|(index, stop)| (stop.id.clone(), index as u32))
        .collect::<HashMap<_, _>>();
    let bindings = provider_bindings(
        &routes,
        &services,
        trips
            .iter()
            .map(|trip| (trip.id.as_str(), trip.route_index)),
        &trips_out,
        &stop_position_by_id,
        &stops,
        &route_by_via_id,
    );
    drop(trips);
    drop(shapes);
    drop(frequencies);
    drop(stop_times);
    diagnostics.scheduled_trips_loaded = trips_out.len();
    diagnostics.frequencies_loaded = frequencies_out.len();
    diagnostics.exact_static_timing_patterns = timing_classes.0;
    diagnostics.derived_bus_timing_patterns = timing_classes.1;
    diagnostics.geometric_fallback_patterns = timing_classes.2;
    for service in &services {
        let mode = routes[*route_by_via_id
            .get(service.route_id.as_str())
            .expect("normalized route")]
        .mode;
        *diagnostics
            .patterns_loaded_by_mode
            .entry(mode_name(mode).to_owned())
            .or_default() += 1;
    }
    let unresolved_bus_scopes = services
        .iter()
        .filter(|service| {
            let route = &routes[*route_by_via_id
                .get(service.route_id.as_str())
                .expect("normalized route")];
            route.mode == TransitMode::Bus
                && bus_realtime_scope(
                    route,
                    &service
                        .stops
                        .iter()
                        .filter_map(|id| stop_position_by_id.get(id).copied())
                        .collect::<Vec<_>>(),
                    &stops,
                )
                .is_none()
        })
        .map(|service| service.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.unresolved_bus_realtime_scope_patterns = unresolved_bus_scopes.len();
    diagnostics.unresolved_bus_realtime_scope_examples =
        unresolved_bus_scopes.into_iter().take(20).collect();
    let used_stop_ids = services
        .iter()
        .flat_map(|service| service.stops.iter().cloned())
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
    let unmapped_services = services
        .iter()
        .filter(|service| {
            !bindings.bus_patterns.contains_key(&service.id)
                && !bindings.metro_patterns.contains_key(&service.id)
                && !matches!(
                    route_by_via_id
                        .get(service.route_id.as_str())
                        .map(|index| routes[*index].mode),
                    Some(TransitMode::Rail | TransitMode::HighSpeedRail)
                )
        })
        .map(|service| service.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.gtfs_pattern_ids_without_tdx_mapping = unmapped_services.len();
    diagnostics.unmapped_pattern_id_examples = unmapped_services.into_iter().take(20).collect();

    services.shrink_to_fit();
    frequencies_out.shrink_to_fit();
    let used_calendars = trips_out
        .iter()
        .map(|trip| trip.calendar_id.as_str().to_owned())
        .chain(
            frequencies_out
                .iter()
                .map(|frequency| frequency.calendar_id.as_str().to_owned()),
        )
        .collect::<HashSet<_>>();
    let network = TransitNetwork::new(default_service_day_boundary(), stops, services, trips_out)
        .with_calendars(
            calendars
                .into_iter()
                .filter(|(id, _)| used_calendars.contains(id.as_str()))
                .map(|(id, calendar)| ServiceCalendar {
                    id: ServiceCalendarId::new(id),
                    weekdays: calendar.weekdays,
                    start_date: calendar.start_date,
                    end_date: calendar.end_date,
                    added_dates: calendar.added,
                    removed_dates: calendar.removed,
                })
                .collect(),
        )
        .with_routes(
            routes
                .into_iter()
                .map(|route| (via_route_id(&route), route))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .map(|(id, route)| {
                    let label = match route.mode {
                        TransitMode::Bus => route.label,
                        TransitMode::Metro => id.strip_prefix("metro:").unwrap_or(&id).to_owned(),
                        TransitMode::Rail => "TRA".to_owned(),
                        TransitMode::HighSpeedRail => "THSR".to_owned(),
                    };
                    TransitRoute {
                        id: TransitRouteId::new(id),
                        mode: route.mode,
                        label,
                    }
                })
                .collect(),
        )
        .with_frequencies(frequencies_out)
        .canonicalize();

    (network, bindings, diagnostics)
}

fn normalize_patterns(
    stops: &[TransitStop],
    routes: &[Route],
    trips: &[TripMeta],
    calendars: &HashMap<String, GtfsCalendar>,
    frequency_rows: &HashMap<u32, Vec<FrequencyRow>>,
    stop_times: &[CompactStopTime],
    shapes: &HashMap<String, Vec<ShapePoint>>,
) -> NormalizedPatterns {
    let mut builders = Vec::<ServiceBuilder>::new();
    let mut service_indices = HashMap::<ServicePatternKey, usize>::new();
    let mut invalid_timing_trip_count = 0;
    let mut invalid_timing_trip_examples = Vec::new();
    let mut start = 0;
    while start < stop_times.len() {
        let trip_index = stop_times[start].trip_index;
        let mut end = start + 1;
        while end < stop_times.len() && stop_times[end].trip_index == trip_index {
            end += 1;
        }
        let trip = &trips[trip_index as usize];
        let rows = &stop_times[start..end];
        let mut ordered = rows.to_vec();
        ordered.sort_by_key(|row| row.sequence);
        let mut pattern = ordered.iter().map(|row| row.stop_index).collect::<Vec<_>>();
        pattern.dedup();
        if pattern.len() >= 2 && pattern.len() == ordered.len() {
            let key = ServicePatternKey {
                route_id: via_route_id(&routes[trip.route_index]),
                direction: trip.direction,
                pattern: pattern.clone(),
            };
            let service_index = *service_indices.entry(key.clone()).or_insert_with(|| {
                builders.push(ServiceBuilder::new(&key, trip.route_index));
                builders.len() - 1
            });
            let builder = &mut builders[service_index];
            builder.add_headsign(&trip.headsign);
            let calendar = calendars
                .get(&trip.gtfs_service_id)
                .cloned()
                .unwrap_or_default();
            let complete_times = ordered
                .iter()
                .all(|row| row.arrival_seconds.is_some() && row.departure_seconds.is_some());
            let template = ordered
                .iter()
                .map(|row| {
                    ScheduledStopTime::new(
                        row.arrival_seconds.unwrap_or_default(),
                        row.departure_seconds.unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            let chronological = complete_times && is_chronological_times(&template);
            if complete_times && !chronological {
                invalid_timing_trip_count += 1;
                if invalid_timing_trip_examples.len() < 20 {
                    invalid_timing_trip_examples.push(trip.id.clone());
                }
            }
            let has_frequencies = frequency_rows
                .get(&trip_index)
                .is_some_and(|frequency_rows| !frequency_rows.is_empty());
            if chronological && !has_frequencies && calendar_is_usable(&calendar) {
                let scheduled = TransitTrip::new(
                    TransitTripId::new(trip.id.clone()),
                    TransitPatternId::new("pending"),
                    ServiceCalendarId::new(trip.gtfs_service_id.clone()),
                    template.clone(),
                );
                builder.scheduled_trips.push(scheduled);
            }
            if let Some(rows) = frequency_rows.get(&trip_index) {
                if chronological && calendar_is_usable(&calendar) {
                    for (frequency_index, frequency) in rows.iter().enumerate() {
                        let normalized = TransitFrequency {
                            id: TransitTripId::new(format!(
                                "{}::frequency={frequency_index}",
                                trip.id
                            )),
                            pattern_id: TransitPatternId::new("pending"),
                            calendar_id: ServiceCalendarId::new(trip.gtfs_service_id.clone()),
                            start_seconds: frequency.start_seconds,
                            end_seconds: frequency.end_seconds,
                            headway_seconds: frequency.headway_seconds,
                            exact_times: frequency.exact_times,
                            template_stop_times: template.clone(),
                        };
                        builder.frequencies.push(normalized);
                    }
                }
            }
            if routes[trip.route_index].mode == TransitMode::Bus {
                if let (Some(first_departure), Some(last_arrival)) = (
                    ordered.first().and_then(|row| row.departure_seconds),
                    ordered.last().and_then(|row| row.arrival_seconds),
                ) {
                    if last_arrival > first_departure {
                        let distances = trip_segment_distances(stops, &ordered, trip, shapes);
                        if let Some(samples) =
                            distribute_runtime(last_arrival - first_departure, &distances)
                        {
                            for (segment, seconds) in
                                builder.bus_segment_samples.iter_mut().zip(samples)
                            {
                                segment.push(seconds);
                            }
                        }
                    }
                }
            }
        }
        start = end;
    }

    let mut groups = BTreeMap::<(String, Option<u8>), Vec<usize>>::new();
    for (index, builder) in builders.iter().enumerate() {
        groups
            .entry((builder.route_id.clone(), builder.direction))
            .or_default()
            .push(index);
    }
    let mut service_ids = vec![TransitPatternId::new(""); builders.len()];
    let mut services = Vec::with_capacity(builders.len());
    for ((route_id, direction), indices) in groups {
        let route = &routes[builders[indices[0]].route_index];
        let mut indices = indices;
        indices.sort_by(|left, right| builders[*left].pattern.cmp(&builders[*right].pattern));
        for (position, builder_index) in indices.iter().enumerate() {
            let builder = &builders[*builder_index];
            let id = TransitPatternId::new(format!(
                "pattern:{route_id}::direction={}::variant={position}",
                direction.map_or("unknown".to_owned(), |value| value.to_string())
            ));
            service_ids[*builder_index] = id.clone();
            let pattern = builder
                .pattern
                .iter()
                .map(|index| stops[*index as usize].id.clone())
                .collect();
            let service = TransitPattern::new(
                id,
                TransitRouteId::new(route_id.clone()),
                direction,
                builder.headsign(route),
                pattern,
                profile_for_builder(
                    &builder.pattern,
                    &builder.scheduled_trips,
                    &builder.frequencies,
                    &builder.bus_segment_samples,
                    route.mode,
                    stops,
                ),
            );
            services.push(service);
        }
    }

    let mut scheduled_trips = Vec::new();
    let mut normalized_frequencies = Vec::new();
    let exact_count = services
        .iter()
        .filter(|pattern| {
            pattern
                .static_profile
                .segments
                .iter()
                .all(|segment| segment.source == SegmentTimeSource::Schedule)
        })
        .count();
    let derived_count = services
        .iter()
        .filter(|pattern| {
            pattern
                .static_profile
                .segments
                .iter()
                .all(|segment| segment.source == SegmentTimeSource::Derived)
        })
        .count();
    let geometric_count = services.len() - exact_count - derived_count;
    for (index, builder) in builders.into_iter().enumerate() {
        let service_id = service_ids[index].clone();
        for mut trip in builder.scheduled_trips {
            trip.pattern_id = service_id.clone();
            scheduled_trips.push(trip);
        }
        for mut frequency in builder.frequencies {
            frequency.pattern_id = service_id.clone();
            normalized_frequencies.push(frequency);
        }
    }
    (
        services,
        scheduled_trips,
        normalized_frequencies,
        (exact_count, derived_count, geometric_count),
        (invalid_timing_trip_count, invalid_timing_trip_examples),
    )
}
