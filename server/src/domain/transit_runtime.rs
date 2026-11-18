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

    pub fn stop(&self, id: &BoardingPointId) -> Option<&TransitStop> {
        let index = *self.indexes.lookup.stop_by_id.get(id)?;
        self.network.stops.get(index)
    }

    pub fn pattern(&self, id: &TransitPatternId) -> Option<&TransitPattern> {
        let index = *self.indexes.lookup.pattern_by_id.get(id)?;
        self.network.patterns.get(index)
    }

    pub(crate) fn required_pattern(&self, id: &TransitPatternId) -> &TransitPattern {
        self.pattern(id).expect("validated transit pattern id")
    }

    pub fn route_for_pattern(&self, pattern: &TransitPattern) -> Option<&TransitRoute> {
        let index = *self.indexes.lookup.route_by_id.get(&pattern.route_id)?;
        self.network.routes.get(index)
    }

    pub fn route_of(&self, pattern: &TransitPattern) -> &TransitRoute {
        self.route_for_pattern(pattern)
            .expect("validated pattern route")
    }

    pub fn calendar(&self, id: &ServiceCalendarId) -> Option<&ServiceCalendar> {
        let index = *self.indexes.lookup.calendar_by_id.get(id)?;
        self.network.calendars.get(index)
    }

    pub fn scheduled_trip(&self, id: &TransitTripId) -> Option<&TransitTrip> {
        let index = *self.indexes.lookup.trip_by_id.get(id)?;
        self.network.trips.get(index)
    }

    pub fn scheduled_trip_at(&self, index: usize) -> Option<&TransitTrip> {
        self.network.trips.get(index)
    }

    pub fn scheduled_trip_indices(
        &self,
        pattern_id: &TransitPatternId,
        board_index: usize,
    ) -> &[usize] {
        self.indexes
            .schedule
            .trip_indices_by_pattern_stop
            .get(pattern_id)
            .and_then(|by_stop| by_stop.get(board_index))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn frequency_at(&self, index: usize) -> Option<&TransitFrequency> {
        self.network.frequencies.get(index)
    }

    pub fn frequency_indices(&self, pattern_id: &TransitPatternId) -> &[usize] {
        self.indexes
            .schedule
            .frequency_indices_by_pattern
            .get(pattern_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
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
