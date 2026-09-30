use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

use super::{
    ids::{BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId},
    location::{GeoCoordinate, METRES_PER_DEGREE},
    normalize_to_taipei, taipei_offset,
    transit::{
        ServiceCalendar, TransitFrequency, TransitNetwork, TransitPattern, TransitRoute,
        TransitStop, TransitTrip,
    },
};

const SPATIAL_CELL_DEGREES: f64 = 0.005;
const MAX_SPATIAL_SEARCH_RING: i32 = 32;

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

    pub fn data(&self) -> &TransitNetwork {
        &self.network
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TransitRuntimeIndexes {
    spatial: SpatialStopIndex,
    lookup: TransitLookupIndex,
    schedule: ScheduleIndex,
}

impl TransitRuntimeIndexes {
    fn build(network: &TransitNetwork) -> Self {
        let mut indexes = Self::default();
        for (index, stop) in network.stops.iter().enumerate() {
            indexes.spatial.stop_grid.insert(stop.coordinate, index);
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
        nearest_from_grid(&self.spatial.stop_grid, &network.stops, coordinate, limit)
    }

    fn stops_within_radius<'a>(
        &self,
        network: &'a TransitNetwork,
        coordinate: GeoCoordinate,
        radius_metres: u32,
    ) -> Vec<&'a TransitStop> {
        points_within_radius_from_grid(
            &self.spatial.stop_grid,
            &network.stops,
            coordinate,
            radius_metres,
        )
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
struct SpatialStopIndex {
    stop_grid: SpatialGrid,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SpatialGrid {
    cells: HashMap<SpatialCell, Vec<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SpatialCell {
    latitude: i32,
    longitude: i32,
}

impl SpatialGrid {
    fn insert(&mut self, coordinate: GeoCoordinate, index: usize) {
        self.cells
            .entry(SpatialCell::from_coordinate(coordinate))
            .or_default()
            .push(index);
    }
}

impl SpatialCell {
    fn from_coordinate(coordinate: GeoCoordinate) -> Self {
        Self {
            latitude: (coordinate.latitude / SPATIAL_CELL_DEGREES).floor() as i32,
            longitude: (coordinate.longitude / SPATIAL_CELL_DEGREES).floor() as i32,
        }
    }
}

trait SpatialPoint {
    fn coordinate(&self) -> GeoCoordinate;
    fn stable_id(&self) -> &str;
}

impl SpatialPoint for TransitStop {
    fn coordinate(&self) -> GeoCoordinate {
        self.coordinate
    }

    fn stable_id(&self) -> &str {
        self.id.as_str()
    }
}

fn nearest_from_grid<'a, T: SpatialPoint>(
    grid: &SpatialGrid,
    points: &'a [T],
    coordinate: GeoCoordinate,
    limit: usize,
) -> Vec<&'a T> {
    if limit == 0 || points.is_empty() {
        return Vec::new();
    }
    let origin = SpatialCell::from_coordinate(coordinate);
    let mut indices = HashSet::new();
    let mut visited_cells = HashSet::new();
    for ring in 0..=MAX_SPATIAL_SEARCH_RING {
        for latitude in (origin.latitude - ring)..=(origin.latitude + ring) {
            for longitude in (origin.longitude - ring)..=(origin.longitude + ring) {
                if ring != 0
                    && (latitude - origin.latitude).abs() != ring
                    && (longitude - origin.longitude).abs() != ring
                {
                    continue;
                }
                let spatial_cell = SpatialCell {
                    latitude,
                    longitude,
                };
                visited_cells.insert(spatial_cell);
                if let Some(cell) = grid.cells.get(&spatial_cell) {
                    indices.extend(cell.iter().copied());
                }
            }
        }
        if indices.len() >= limit || ring == MAX_SPATIAL_SEARCH_RING {
            break;
        }
    }
    if indices.len() < limit {
        indices.extend(0..points.len());
    } else {
        loop {
            let worst_distance = indices
                .iter()
                .filter_map(|index| points.get(*index))
                .map(|point| coordinate.planar_distance_metres(point.coordinate()))
                .max()
                .unwrap_or(0);
            let mut added_cell = false;
            for (cell, cell_indices) in &grid.cells {
                if visited_cells.contains(cell)
                    || cell_lower_bound_metres(coordinate, *cell) > worst_distance
                {
                    continue;
                }
                visited_cells.insert(*cell);
                indices.extend(cell_indices.iter().copied());
                added_cell = true;
            }
            if !added_cell {
                break;
            }
        }
    }
    let mut nearest = indices
        .into_iter()
        .filter_map(|index| points.get(index))
        .collect::<Vec<_>>();
    nearest.sort_by(|left, right| {
        coordinate
            .planar_distance_metres(left.coordinate())
            .cmp(&coordinate.planar_distance_metres(right.coordinate()))
            .then(left.stable_id().cmp(right.stable_id()))
    });
    nearest.truncate(limit);
    nearest
}

fn points_within_radius_from_grid<'a, T: SpatialPoint>(
    grid: &SpatialGrid,
    points: &'a [T],
    coordinate: GeoCoordinate,
    radius_metres: u32,
) -> Vec<&'a T> {
    if points.is_empty() {
        return Vec::new();
    }

    let latitude_delta = f64::from(radius_metres) / METRES_PER_DEGREE;
    let longitude_scale =
        METRES_PER_DEGREE * coordinate.latitude.to_radians().cos().abs().max(0.01);
    let longitude_delta = f64::from(radius_metres) / longitude_scale;
    let latitude_min = SpatialCell::from_coordinate(GeoCoordinate {
        latitude: coordinate.latitude - latitude_delta,
        longitude: coordinate.longitude,
    })
    .latitude;
    let latitude_max = SpatialCell::from_coordinate(GeoCoordinate {
        latitude: coordinate.latitude + latitude_delta,
        longitude: coordinate.longitude,
    })
    .latitude;
    let longitude_min = SpatialCell::from_coordinate(GeoCoordinate {
        latitude: coordinate.latitude,
        longitude: coordinate.longitude - longitude_delta,
    })
    .longitude;
    let longitude_max = SpatialCell::from_coordinate(GeoCoordinate {
        latitude: coordinate.latitude,
        longitude: coordinate.longitude + longitude_delta,
    })
    .longitude;
    let mut indices = HashSet::new();
    for latitude in latitude_min..=latitude_max {
        for longitude in longitude_min..=longitude_max {
            if let Some(cell) = grid.cells.get(&SpatialCell {
                latitude,
                longitude,
            }) {
                indices.extend(cell.iter().copied());
            }
        }
    }

    let mut nearby = indices
        .into_iter()
        .filter_map(|index| points.get(index))
        .filter(|point| {
            coordinate.planar_distance_metres(point.coordinate()) <= u64::from(radius_metres)
        })
        .collect::<Vec<_>>();
    nearby.sort_by(|left, right| {
        coordinate
            .planar_distance_metres(left.coordinate())
            .cmp(&coordinate.planar_distance_metres(right.coordinate()))
            .then(left.stable_id().cmp(right.stable_id()))
    });
    nearby
}

fn cell_lower_bound_metres(coordinate: GeoCoordinate, cell: SpatialCell) -> u64 {
    let latitude_min = f64::from(cell.latitude) * SPATIAL_CELL_DEGREES;
    let latitude_max = latitude_min + SPATIAL_CELL_DEGREES;
    let longitude_min = f64::from(cell.longitude) * SPATIAL_CELL_DEGREES;
    let longitude_max = longitude_min + SPATIAL_CELL_DEGREES;
    let latitude = coordinate.latitude.clamp(latitude_min, latitude_max);
    let longitude = coordinate.longitude.clamp(longitude_min, longitude_max);
    coordinate.planar_distance_metres(GeoCoordinate {
        latitude,
        longitude,
    })
}
