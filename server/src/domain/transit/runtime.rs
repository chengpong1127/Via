use std::{collections::HashMap, sync::Arc};

use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

use crate::domain::{
    ids::{BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId},
    location::GeoCoordinate,
    normalize_to_taipei,
    spatial::SpatialPointIndex,
    taipei_offset,
    transit::network::{
        ServiceCalendar, TransferEdge, TransitFrequency, TransitNetwork,
        TransitNetworkValidationError, TransitPattern, TransitRoute, TransitStop, TransitTrip,
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PatternPosition {
    pub pattern_id: TransitPatternId,
    pub stop_index: usize,
}

#[derive(Clone, Debug)]
pub struct TransitRuntime {
    network: Arc<TransitNetwork>,
    indexes: Arc<TransitRuntimeIndexes>,
}

impl TransitRuntime {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        let indexes = Arc::new(TransitRuntimeIndexes::build(&network));
        Self { network, indexes }
    }

    pub fn network(&self) -> &TransitNetwork {
        &self.network
    }

    pub fn has_revisions(&self) -> bool {
        self.network.has_revisions()
    }

    pub fn validate(&self) -> Result<(), TransitNetworkValidationError> {
        self.network.validate()
    }
}

impl TransitRuntime {
    pub fn stop(&self, id: &BoardingPointId) -> Option<&TransitStop> {
        entity(
            &self.network.stops,
            self.indexes.lookup.position_by_stop_id.get(id),
        )
    }

    pub fn pattern(&self, id: &TransitPatternId) -> Option<&TransitPattern> {
        entity(
            &self.network.patterns,
            self.indexes.lookup.position_by_pattern_id.get(id),
        )
    }

    pub(crate) fn required_pattern(&self, id: &TransitPatternId) -> &TransitPattern {
        self.pattern(id).expect("validated transit pattern id")
    }

    pub fn required_route(&self, id: &TransitRouteId) -> &TransitRoute {
        entity(
            &self.network.routes,
            self.indexes.lookup.position_by_route_id.get(id),
        )
        .expect("validated transit route id")
    }

    pub fn calendar(&self, id: &ServiceCalendarId) -> Option<&ServiceCalendar> {
        entity(
            &self.network.calendars,
            self.indexes.lookup.position_by_calendar_id.get(id),
        )
    }

    pub fn scheduled_trip(&self, id: &TransitTripId) -> Option<&TransitTrip> {
        entity(
            &self.network.trips,
            self.indexes.lookup.position_by_trip_id.get(id),
        )
    }
}

impl TransitRuntime {
    pub fn stops_within_radius(
        &self,
        coordinate: GeoCoordinate,
        radius_metres: u32,
    ) -> Vec<&TransitStop> {
        self.indexes
            .spatial
            .within_radius(&self.network.stops, coordinate, f64::from(radius_metres))
            .into_iter()
            .map(|index| &self.network.stops[index])
            .collect()
    }

    pub fn pattern_positions_at(&self, stop: &BoardingPointId) -> &[PatternPosition] {
        self.indexes.topology.pattern_positions_at(stop)
    }

    pub fn transfers_from(&self, stop: &BoardingPointId) -> &[TransferEdge] {
        self.indexes.topology.transfers_from(stop)
    }
}

impl TransitRuntime {
    pub fn scheduled_trips_of(
        &self,
        pattern_id: &TransitPatternId,
    ) -> impl Iterator<Item = &TransitTrip> {
        entities(
            &self.network.trips,
            self.indexes.schedule.trips_of(pattern_id),
        )
    }

    pub fn scheduled_trips_boarding_at(
        &self,
        pattern_id: &TransitPatternId,
        board_index: usize,
    ) -> impl Iterator<Item = &TransitTrip> {
        entities(
            &self.network.trips,
            self.indexes
                .schedule
                .trips_boarding_at(pattern_id, board_index),
        )
    }

    pub fn frequencies_of(
        &self,
        pattern_id: &TransitPatternId,
    ) -> impl Iterator<Item = &TransitFrequency> {
        entities(
            &self.network.frequencies,
            self.indexes.schedule.frequencies_of(pattern_id),
        )
    }
}

impl TransitRuntime {
    pub fn same_stop_transfer_seconds(&self) -> u32 {
        self.network.same_stop_transfer_seconds
    }

    pub fn service_day(&self, instant: OffsetDateTime) -> Date {
        let local = normalize_to_taipei(instant);
        let local = local.date().with_time(local.time());
        if local.time() < self.network.service_day_boundary {
            local.date().previous_day().unwrap_or(local.date())
        } else {
            local.date()
        }
    }

    pub fn service_day_start(&self, service_day: Date) -> OffsetDateTime {
        PrimitiveDateTime::new(service_day, Time::MIDNIGHT).assume_offset(taipei_offset())
    }
}

fn entity<'a, T>(items: &'a [T], index: Option<&usize>) -> Option<&'a T> {
    items.get(*index?)
}

fn entities<'a, T>(items: &'a [T], indices: &'a [usize]) -> impl Iterator<Item = &'a T> {
    indices.iter().map(|index| &items[*index])
}

#[derive(Clone, Debug, Default)]
struct TransitRuntimeIndexes {
    spatial: SpatialPointIndex,
    lookup: LookupIndex,
    schedule: ScheduleIndex,
    topology: TopologyIndex,
}

impl TransitRuntimeIndexes {
    fn build(network: &TransitNetwork) -> Self {
        Self {
            spatial: SpatialPointIndex::build(&network.stops),
            lookup: LookupIndex::build(network),
            schedule: ScheduleIndex::build(network),
            topology: TopologyIndex::build(network),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LookupIndex {
    position_by_stop_id: HashMap<BoardingPointId, usize>,
    position_by_route_id: HashMap<TransitRouteId, usize>,
    position_by_pattern_id: HashMap<TransitPatternId, usize>,
    position_by_trip_id: HashMap<TransitTripId, usize>,
    position_by_calendar_id: HashMap<ServiceCalendarId, usize>,
}

impl LookupIndex {
    fn build(network: &TransitNetwork) -> Self {
        Self {
            position_by_stop_id: position_by_key(&network.stops, |stop| stop.id.clone()),
            position_by_route_id: position_by_key(&network.routes, |route| route.id.clone()),
            position_by_pattern_id: position_by_key(&network.patterns, |pattern| {
                pattern.id.clone()
            }),
            position_by_trip_id: position_by_key(&network.trips, |trip| trip.id.clone()),
            position_by_calendar_id: position_by_key(&network.calendars, |calendar| {
                calendar.id.clone()
            }),
        }
    }
}

fn position_by_key<T, K: std::hash::Hash + Eq>(
    items: &[T],
    key: impl Fn(&T) -> K,
) -> HashMap<K, usize> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| (key(item), index))
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScheduleIndex {
    trips_by_pattern: HashMap<TransitPatternId, Vec<usize>>,
    trips_by_pattern_stop: HashMap<TransitPatternId, Vec<Vec<usize>>>,
    frequencies_by_pattern: HashMap<TransitPatternId, Vec<usize>>,
}

impl ScheduleIndex {
    fn build(network: &TransitNetwork) -> Self {
        let pattern_by_id = network
            .patterns
            .iter()
            .map(|pattern| (&pattern.id, pattern))
            .collect::<HashMap<_, _>>();
        let mut index = Self::default();
        for (trip_index, trip) in network.trips.iter().enumerate() {
            index
                .trips_by_pattern
                .entry(trip.pattern_id.clone())
                .or_default()
                .push(trip_index);
            let Some(pattern) = pattern_by_id.get(&trip.pattern_id) else {
                continue;
            };
            if trip.stop_times.len() != pattern.stops.len() {
                continue;
            }
            let by_stop = index
                .trips_by_pattern_stop
                .entry(trip.pattern_id.clone())
                .or_insert_with(|| vec![Vec::new(); pattern.stops.len()]);
            for trips_at_stop in by_stop.iter_mut() {
                trips_at_stop.push(trip_index);
            }
        }
        for by_stop in index.trips_by_pattern_stop.values_mut() {
            for (board_index, trips_at_stop) in by_stop.iter_mut().enumerate() {
                trips_at_stop.sort_by(|left, right| {
                    let left_trip = &network.trips[*left];
                    let right_trip = &network.trips[*right];
                    left_trip.stop_times[board_index]
                        .departure_seconds
                        .cmp(&right_trip.stop_times[board_index].departure_seconds)
                        .then(left_trip.id.as_str().cmp(right_trip.id.as_str()))
                });
            }
        }
        for (frequency_index, frequency) in network.frequencies.iter().enumerate() {
            index
                .frequencies_by_pattern
                .entry(frequency.pattern_id.clone())
                .or_default()
                .push(frequency_index);
        }
        index
    }

    fn trips_of(&self, pattern_id: &TransitPatternId) -> &[usize] {
        self.trips_by_pattern
            .get(pattern_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn trips_boarding_at(&self, pattern_id: &TransitPatternId, board_index: usize) -> &[usize] {
        self.trips_by_pattern_stop
            .get(pattern_id)
            .and_then(|by_stop| by_stop.get(board_index))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn frequencies_of(&self, pattern_id: &TransitPatternId) -> &[usize] {
        self.frequencies_by_pattern
            .get(pattern_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TopologyIndex {
    pattern_positions_by_stop: HashMap<BoardingPointId, Vec<PatternPosition>>,
    transfers_by_stop: HashMap<BoardingPointId, Vec<TransferEdge>>,
}

impl TopologyIndex {
    fn build(network: &TransitNetwork) -> Self {
        let mut index = Self::default();
        for pattern in &network.patterns {
            for (stop_index, stop_id) in pattern.stops.iter().enumerate() {
                index
                    .pattern_positions_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(PatternPosition {
                        pattern_id: pattern.id.clone(),
                        stop_index,
                    });
            }
        }
        for positions in index.pattern_positions_by_stop.values_mut() {
            positions.sort_by(|left, right| {
                left.pattern_id
                    .as_str()
                    .cmp(right.pattern_id.as_str())
                    .then(left.stop_index.cmp(&right.stop_index))
            });
        }
        for edge in &network.transfer_edges {
            let outgoing = index
                .transfers_by_stop
                .entry(edge.from.clone())
                .or_default();
            if !outgoing.contains(edge) {
                outgoing.push(edge.clone());
            }
        }
        for outgoing in index.transfers_by_stop.values_mut() {
            outgoing.sort_by(|left, right| {
                left.to
                    .as_str()
                    .cmp(right.to.as_str())
                    .then(left.duration_seconds.cmp(&right.duration_seconds))
            });
        }
        index
    }

    fn pattern_positions_at(&self, stop: &BoardingPointId) -> &[PatternPosition] {
        self.pattern_positions_by_stop
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn transfers_from(&self, stop: &BoardingPointId) -> &[TransferEdge] {
        self.transfers_by_stop
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
