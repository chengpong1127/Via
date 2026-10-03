use std::{collections::HashMap, sync::Arc};

use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

use super::{
    ids::{BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId},
    location::GeoCoordinate,
    normalize_to_taipei,
    spatial::SpatialPointIndex,
    taipei_offset,
    transit::{
        ServiceCalendar, TransitFrequency, TransitNetwork, TransitNetworkValidationError,
        TransitPattern, TransitRoute, TransitStop, TransitTrip,
    },
};

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

    pub fn revisions(&self) -> (&str, &str) {
        self.network.revisions()
    }

    pub fn validate(&self) -> Result<(), TransitNetworkValidationError> {
        self.network.validate()
    }

    pub fn nearest_stops(&self, coordinate: GeoCoordinate, limit: usize) -> Vec<&TransitStop> {
        self.indexes.nearest_stops(&self.network, coordinate, limit)
    }

    pub fn stops_within_radius(
        &self,
        coordinate: GeoCoordinate,
        radius_metres: u32,
    ) -> Vec<&TransitStop> {
        self.indexes
            .stops_within_radius(&self.network, coordinate, radius_metres)
    }

    pub fn stop(&self, id: &BoardingPointId) -> Option<&TransitStop> {
        self.indexes.stop(&self.network, id)
    }

    pub fn pattern(&self, id: &TransitPatternId) -> Option<&TransitPattern> {
        self.indexes.pattern(&self.network, id)
    }

    pub fn route_for_pattern(&self, pattern: &TransitPattern) -> Option<&TransitRoute> {
        self.indexes.route_for_pattern(&self.network, pattern)
    }

    pub fn route_of(&self, pattern: &TransitPattern) -> &TransitRoute {
        self.indexes.route_of(&self.network, pattern)
    }

    pub fn calendar(&self, id: &ServiceCalendarId) -> Option<&ServiceCalendar> {
        self.indexes.calendar(&self.network, id)
    }

    pub fn scheduled_trip(&self, id: &TransitTripId) -> Option<&TransitTrip> {
        self.indexes.scheduled_trip(&self.network, id)
    }

    pub fn scheduled_trip_at(&self, index: usize) -> Option<&TransitTrip> {
        self.indexes.scheduled_trip_at(&self.network, index)
    }

    pub fn scheduled_trip_indices(
        &self,
        pattern_id: &TransitPatternId,
        board_index: usize,
    ) -> &[usize] {
        self.indexes.scheduled_trip_indices(pattern_id, board_index)
    }

    pub fn frequency_at(&self, index: usize) -> Option<&TransitFrequency> {
        self.network.frequencies.get(index)
    }

    pub fn frequency_indices(&self, pattern_id: &TransitPatternId) -> &[usize] {
        self.indexes.frequency_indices(pattern_id)
    }

    pub fn same_stop_transfer_seconds(&self) -> u32 {
        self.network.default_transfer_seconds
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

#[derive(Clone, Debug, Default)]
struct TransitRuntimeIndexes {
    spatial: SpatialPointIndex,
    lookup: TransitLookupIndex,
    schedule: ScheduleIndex,
}

impl TransitRuntimeIndexes {
    fn build(network: &TransitNetwork) -> Self {
        let mut indexes = Self {
            spatial: SpatialPointIndex::build(&network.stops),
            ..Self::default()
        };
        for (index, stop) in network.stops.iter().enumerate() {
            indexes.lookup.stop_by_id.insert(stop.id.clone(), index);
        }
        for (index, route) in network.routes.iter().enumerate() {
            indexes.lookup.route_by_id.insert(route.id.clone(), index);
        }
        for (index, calendar) in network.calendars.iter().enumerate() {
            indexes
                .lookup
                .calendar_by_id
                .insert(calendar.id.clone(), index);
        }
        for (index, pattern) in network.patterns.iter().enumerate() {
            indexes
                .lookup
                .pattern_by_id
                .insert(pattern.id.clone(), index);
        }
        for (trip_index, trip) in network.trips.iter().enumerate() {
            indexes
                .lookup
                .trip_by_id
                .insert(trip.id.clone(), trip_index);
            let Some(pattern_index) = indexes.lookup.pattern_by_id.get(&trip.pattern_id).copied()
            else {
                continue;
            };
            let pattern = &network.patterns[pattern_index];
            if trip.stop_times.len() != pattern.stops.len() {
                continue;
            }
            let trip_indices = indexes
                .schedule
                .trip_indices_by_pattern_stop
                .entry(trip.pattern_id.clone())
                .or_insert_with(|| vec![Vec::new(); pattern.stops.len()]);
            for trip_indices_at_stop in trip_indices.iter_mut().take(trip.stop_times.len()) {
                trip_indices_at_stop.push(trip_index);
            }
        }
        for trip_indices_by_stop in indexes.schedule.trip_indices_by_pattern_stop.values_mut() {
            for (board_index, trip_indices) in trip_indices_by_stop.iter_mut().enumerate() {
                trip_indices.sort_by(|left, right| {
                    let left_trip = &network.trips[*left];
                    let right_trip = &network.trips[*right];
                    left_trip.stop_times[board_index]
                        .departure_seconds
                        .cmp(&right_trip.stop_times[board_index].departure_seconds)
                        .then(left_trip.id.as_str().cmp(right_trip.id.as_str()))
                });
            }
        }
        for (index, frequency) in network.frequencies.iter().enumerate() {
            indexes
                .schedule
                .frequency_indices_by_pattern
                .entry(frequency.pattern_id.clone())
                .or_default()
                .push(index);
        }
        indexes
    }

    fn nearest_stops<'a>(
        &self,
        network: &'a TransitNetwork,
        coordinate: GeoCoordinate,
        limit: usize,
    ) -> Vec<&'a TransitStop> {
        self.spatial
            .nearest(&network.stops, coordinate, limit)
            .into_iter()
            .map(|(index, _)| &network.stops[index])
            .collect()
    }

    fn stops_within_radius<'a>(
        &self,
        network: &'a TransitNetwork,
        coordinate: GeoCoordinate,
        radius_metres: u32,
    ) -> Vec<&'a TransitStop> {
        self.spatial
            .within_radius(&network.stops, coordinate, f64::from(radius_metres))
            .into_iter()
            .map(|(index, _)| &network.stops[index])
            .collect()
    }

    fn stop<'a>(
        &self,
        network: &'a TransitNetwork,
        id: &BoardingPointId,
    ) -> Option<&'a TransitStop> {
        let index = self.lookup.stop_by_id.get(id).copied()?;
        network.stops.get(index)
    }

    fn pattern<'a>(
        &self,
        network: &'a TransitNetwork,
        id: &TransitPatternId,
    ) -> Option<&'a TransitPattern> {
        let index = self.lookup.pattern_by_id.get(id).copied()?;
        network.patterns.get(index)
    }

    fn route_for_pattern<'a>(
        &self,
        network: &'a TransitNetwork,
        pattern: &TransitPattern,
    ) -> Option<&'a TransitRoute> {
        let index = self.lookup.route_by_id.get(&pattern.route_id).copied()?;
        network.routes.get(index)
    }

    fn route_of<'a>(
        &self,
        network: &'a TransitNetwork,
        pattern: &TransitPattern,
    ) -> &'a TransitRoute {
        self.route_for_pattern(network, pattern)
            .expect("validated pattern route")
    }

    fn calendar<'a>(
        &self,
        network: &'a TransitNetwork,
        id: &ServiceCalendarId,
    ) -> Option<&'a ServiceCalendar> {
        let index = self.lookup.calendar_by_id.get(id).copied()?;
        network.calendars.get(index)
    }

    fn scheduled_trip<'a>(
        &self,
        network: &'a TransitNetwork,
        id: &TransitTripId,
    ) -> Option<&'a TransitTrip> {
        let index = self.lookup.trip_by_id.get(id).copied()?;
        network.trips.get(index)
    }

    fn scheduled_trip_at<'a>(
        &self,
        network: &'a TransitNetwork,
        index: usize,
    ) -> Option<&'a TransitTrip> {
        network.trips.get(index)
    }

    fn scheduled_trip_indices(
        &self,
        pattern_id: &TransitPatternId,
        board_index: usize,
    ) -> &[usize] {
        self.schedule
            .trip_indices_by_pattern_stop
            .get(pattern_id)
            .and_then(|by_stop| by_stop.get(board_index))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn frequency_indices(&self, pattern_id: &TransitPatternId) -> &[usize] {
        self.schedule
            .frequency_indices_by_pattern
            .get(pattern_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TransitLookupIndex {
    stop_by_id: HashMap<BoardingPointId, usize>,
    route_by_id: HashMap<TransitRouteId, usize>,
    pattern_by_id: HashMap<TransitPatternId, usize>,
    trip_by_id: HashMap<TransitTripId, usize>,
    calendar_by_id: HashMap<ServiceCalendarId, usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScheduleIndex {
    trip_indices_by_pattern_stop: HashMap<TransitPatternId, Vec<Vec<usize>>>,
    frequency_indices_by_pattern: HashMap<TransitPatternId, Vec<usize>>,
}
