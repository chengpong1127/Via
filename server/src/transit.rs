//! Immutable timetable data and the small transit portion of Via's planner.
//!
//! This is intentionally independent of TDX (or any other provider). Feed adapters
//! build these records during a static-network refresh, and a planning request only
//! reads the pinned revision. The search is a bounded RAPTOR-style round scan: each
//! round adds at most one transit boarding and retains the parent ride chain.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use time::{Date, OffsetDateTime, PrimitiveDateTime, Time, Weekday};

use crate::{
    api::{JourneyMode, RequestCoordinate, normalize_to_taipei, taipei_offset},
    normalization::{BikeShareStationId, BoardingPointId, ScheduledRunId, TransitServiceId},
};

pub const DEFAULT_MAX_TRANSFERS: usize = 2;
const SPATIAL_CELL_DEGREES: f64 = 0.005;
const MAX_SPATIAL_SEARCH_RING: i32 = 32;

pub fn default_service_day_boundary() -> Time {
    Time::from_hms(4, 0, 0).expect("service-day boundary is valid")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitMode {
    Bus,
    Metro,
    Rail,
    HighSpeedRail,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitStop {
    pub id: BoardingPointId,
    pub name: String,
    pub coordinate: RequestCoordinate,
}

impl TransitStop {
    pub fn new(
        id: BoardingPointId,
        name: impl Into<String>,
        coordinate: RequestCoordinate,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            coordinate,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitService {
    pub id: TransitServiceId,
    pub transit_mode: TransitMode,
    pub label: String,
    pub headsign: String,
    pub pattern: Vec<BoardingPointId>,
    /// True when this operator/capability must provide a fresh Service Condition
    /// before a Journey Option can be displayed.
    pub service_condition_required: bool,
}

impl TransitService {
    pub fn new(
        id: TransitServiceId,
        transit_mode: TransitMode,
        label: impl Into<String>,
        headsign: impl Into<String>,
        pattern: Vec<BoardingPointId>,
    ) -> Self {
        Self {
            id,
            transit_mode,
            label: label.into(),
            headsign: headsign.into(),
            pattern,
            service_condition_required: false,
        }
    }

    pub fn requiring_service_conditions(mut self) -> Self {
        self.service_condition_required = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledStopTime {
    /// Seconds from the beginning of the Service Day. Values may exceed 24 hours.
    pub arrival_seconds: u32,
    /// Seconds from the beginning of the Service Day. Values may exceed 24 hours.
    pub departure_seconds: u32,
}

impl ScheduledStopTime {
    pub fn new(arrival_seconds: u32, departure_seconds: u32) -> Self {
        Self {
            arrival_seconds,
            departure_seconds,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTrip {
    pub id: ScheduledRunId,
    pub service_id: TransitServiceId,
    pub active_weekdays: Vec<Weekday>,
    #[serde(default)]
    pub added_service_days: Vec<Date>,
    #[serde(default)]
    pub removed_service_days: Vec<Date>,
    pub stop_times: Vec<ScheduledStopTime>,
}

impl ScheduledTrip {
    pub fn weekdays(
        id: ScheduledRunId,
        service_id: TransitServiceId,
        active_weekdays: Vec<Weekday>,
        stop_times: Vec<ScheduledStopTime>,
    ) -> Self {
        Self {
            id,
            service_id,
            active_weekdays,
            added_service_days: Vec::new(),
            removed_service_days: Vec::new(),
            stop_times,
        }
    }

    pub fn with_service_day_exceptions(
        mut self,
        added_service_days: Vec<Date>,
        removed_service_days: Vec<Date>,
    ) -> Self {
        self.added_service_days = added_service_days;
        self.removed_service_days = removed_service_days;
        self
    }

    fn runs_on(&self, service_day: Date) -> bool {
        if self.removed_service_days.contains(&service_day) {
            return false;
        }
        self.added_service_days.contains(&service_day)
            || self.active_weekdays.contains(&service_day.weekday())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BikeShareStation {
    pub id: BikeShareStationId,
    pub name: String,
    pub coordinate: RequestCoordinate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEdge {
    pub from: BoardingPointId,
    pub to: BoardingPointId,
    pub duration_seconds: u32,
}

impl TransferEdge {
    pub fn new(from: BoardingPointId, to: BoardingPointId, duration_seconds: u32) -> Self {
        Self {
            from,
            to,
            duration_seconds,
        }
    }
}

impl BikeShareStation {
    pub fn new(
        id: BikeShareStationId,
        name: impl Into<String>,
        coordinate: RequestCoordinate,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            coordinate,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitNetwork {
    pub service_day_boundary: Time,
    pub stops: Vec<TransitStop>,
    pub services: Vec<TransitService>,
    pub trips: Vec<ScheduledTrip>,
    pub bike_share_stations: Vec<BikeShareStation>,
    /// Explicit movement time between distinct Boarding Points. Same-point
    /// transfers use `default_transfer_seconds` for backwards-compatible
    /// interchange dwell time; distinct points must be listed here.
    #[serde(default)]
    pub transfer_edges: Vec<TransferEdge>,
    pub default_transfer_seconds: u32,
    /// Revisions are part of the artifact, not inferred from deployment time.
    #[serde(default)]
    pub static_network_revision: String,
    #[serde(default)]
    pub schedule_revision: String,
    #[serde(skip, default)]
    indexes: Arc<OnceLock<TransitIndexes>>,
}

impl PartialEq for TransitNetwork {
    fn eq(&self, other: &Self) -> bool {
        self.service_day_boundary == other.service_day_boundary
            && self.stops == other.stops
            && self.services == other.services
            && self.trips == other.trips
            && self.bike_share_stations == other.bike_share_stations
            && self.transfer_edges == other.transfer_edges
            && self.default_transfer_seconds == other.default_transfer_seconds
            && self.static_network_revision == other.static_network_revision
            && self.schedule_revision == other.schedule_revision
    }
}

impl Default for TransitNetwork {
    fn default() -> Self {
        Self::new(
            default_service_day_boundary(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }
}

impl TransitNetwork {
    pub fn new(
        service_day_boundary: Time,
        stops: Vec<TransitStop>,
        services: Vec<TransitService>,
        trips: Vec<ScheduledTrip>,
        bike_share_stations: Vec<BikeShareStation>,
    ) -> Self {
        Self {
            service_day_boundary,
            stops,
            services,
            trips,
            bike_share_stations,
            transfer_edges: Vec::new(),
            default_transfer_seconds: 120,
            static_network_revision: String::new(),
            schedule_revision: String::new(),
            indexes: Arc::new(OnceLock::new()),
        }
    }

    pub fn with_default_transfer_seconds(mut self, seconds: u32) -> Self {
        self.default_transfer_seconds = seconds;
        self
    }

    pub fn with_transfer_edges(mut self, transfer_edges: Vec<TransferEdge>) -> Self {
        self.transfer_edges = transfer_edges;
        self
    }

    pub fn with_revisions(
        mut self,
        static_network_revision: impl Into<String>,
        schedule_revision: impl Into<String>,
    ) -> Self {
        self.static_network_revision = static_network_revision.into();
        self.schedule_revision = schedule_revision.into();
        self
    }

    /// Validate an immutable network artifact before accepting traffic.
    pub fn validate(&self) -> Result<(), TransitNetworkValidationError> {
        if self.static_network_revision.trim().is_empty()
            || self.schedule_revision.trim().is_empty()
        {
            return Err(TransitNetworkValidationError::MissingRevision);
        }
        if self.stops.is_empty() {
            return Err(TransitNetworkValidationError::EmptyStops);
        }
        let mut stop_ids = HashSet::new();
        for stop in &self.stops {
            if !stop_ids.insert(stop.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateStop(
                    stop.id.clone(),
                ));
            }
            if !stop.coordinate.latitude.is_finite()
                || !(-90.0..=90.0).contains(&stop.coordinate.latitude)
                || !stop.coordinate.longitude.is_finite()
                || !(-180.0..=180.0).contains(&stop.coordinate.longitude)
            {
                return Err(TransitNetworkValidationError::InvalidStopCoordinate(
                    stop.id.clone(),
                ));
            }
        }
        let mut service_ids = HashSet::new();
        for service in &self.services {
            if !service_ids.insert(service.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateService(
                    service.id.clone(),
                ));
            }
            if service.pattern.len() < 2 {
                return Err(TransitNetworkValidationError::PatternTooShort(
                    service.id.clone(),
                ));
            }
            if service
                .pattern
                .iter()
                .any(|stop_id| !stop_ids.contains(stop_id))
            {
                return Err(TransitNetworkValidationError::UnknownPatternStop(
                    service.id.clone(),
                ));
            }
        }
        for trip in &self.trips {
            let Some(service) = self.service(&trip.service_id) else {
                return Err(TransitNetworkValidationError::UnknownTripService(
                    trip.service_id.clone(),
                ));
            };
            if (trip.active_weekdays.is_empty() && trip.added_service_days.is_empty())
                || trip.stop_times.len() != service.pattern.len()
            {
                return Err(TransitNetworkValidationError::InvalidTripShape(
                    trip.id.clone(),
                ));
            }
            if trip.stop_times.windows(2).any(|times| {
                times[0].arrival_seconds > times[0].departure_seconds
                    || times[0].departure_seconds > times[1].arrival_seconds
            }) {
                return Err(TransitNetworkValidationError::NonChronologicalTrip(
                    trip.id.clone(),
                ));
            }
        }
        let mut trip_ids = HashSet::new();
        for trip in &self.trips {
            if !trip_ids.insert(trip.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateTrip(
                    trip.id.clone(),
                ));
            }
        }
        let mut station_ids = HashSet::new();
        for station in &self.bike_share_stations {
            if !station_ids.insert(station.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateStation(
                    station.id.clone(),
                ));
            }
            if !station.coordinate.latitude.is_finite()
                || !(-90.0..=90.0).contains(&station.coordinate.latitude)
                || !station.coordinate.longitude.is_finite()
                || !(-180.0..=180.0).contains(&station.coordinate.longitude)
            {
                return Err(TransitNetworkValidationError::InvalidStationCoordinate(
                    station.id.clone(),
                ));
            }
        }
        for edge in &self.transfer_edges {
            if !stop_ids.contains(&edge.from) || !stop_ids.contains(&edge.to) {
                return Err(TransitNetworkValidationError::UnknownTransferEndpoint {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                });
            }
        }
        Ok(())
    }

    /// Normalize outer artifact ordering before hashing, storing, or deploying
    /// an artifact. Ordered Service Patterns and stop times are preserved.
    pub fn canonicalize(mut self) -> Self {
        self.stops
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.services
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.trips
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.bike_share_stations
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.transfer_edges.sort_by(|left, right| {
            left.from
                .as_str()
                .cmp(right.from.as_str())
                .then(left.to.as_str().cmp(right.to.as_str()))
                .then(left.duration_seconds.cmp(&right.duration_seconds))
        });
        self.indexes = Arc::new(OnceLock::new());
        self
    }

    pub fn from_artifact_json(input: &str) -> Result<Self, TransitArtifactError> {
        let network = serde_json::from_str::<Self>(input)
            .map_err(|error| TransitArtifactError::InvalidJson(error.to_string()))?
            .canonicalize();
        network
            .validate()
            .map_err(TransitArtifactError::Invalid)
            .map(|_| network)
    }

    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.clone().canonicalize())
    }

    pub fn revisions(&self) -> (&str, &str) {
        (&self.static_network_revision, &self.schedule_revision)
    }

    pub fn matches_snapshot(&self, static_revision: &str, schedule_revision: &str) -> bool {
        !self.static_network_revision.is_empty()
            && !self.schedule_revision.is_empty()
            && self.static_network_revision == static_revision
            && self.schedule_revision == schedule_revision
    }

    /// Return spatially nearest stops in deterministic distance/id order.
    /// The grid avoids scanning all of Taipei for the normal bounded query;
    /// the bounded fallback preserves correctness for sparse fixtures.
    pub fn nearest_stops(&self, coordinate: RequestCoordinate, limit: usize) -> Vec<&TransitStop> {
        nearest_from_grid(&self.indexes().stop_grid, &self.stops, coordinate, limit)
    }

    pub fn nearest_bike_share_stations(
        &self,
        coordinate: RequestCoordinate,
        limit: usize,
    ) -> Vec<&BikeShareStation> {
        nearest_from_grid(
            &self.indexes().station_grid,
            &self.bike_share_stations,
            coordinate,
            limit,
        )
    }

    pub fn transfer_seconds(&self, from: &BoardingPointId, to: &BoardingPointId) -> Option<u32> {
        if from == to {
            return Some(self.default_transfer_seconds);
        }
        self.transfer_edges
            .iter()
            .find(|edge| &edge.from == from && &edge.to == to)
            .map(|edge| edge.duration_seconds)
    }

    pub fn stop(&self, id: &BoardingPointId) -> Option<&TransitStop> {
        self.stops.iter().find(|stop| &stop.id == id)
    }

    pub fn service(&self, id: &TransitServiceId) -> Option<&TransitService> {
        self.services.iter().find(|service| &service.id == id)
    }

    pub fn service_day(&self, instant: OffsetDateTime) -> Date {
        let local = normalize_to_taipei(instant);
        let local = local.date().with_time(local.time());
        if local.time() < self.service_day_boundary {
            local.date().previous_day().unwrap_or(local.date())
        } else {
            local.date()
        }
    }

    pub fn service_day_start(&self, service_day: Date) -> OffsetDateTime {
        PrimitiveDateTime::new(service_day, Time::MIDNIGHT).assume_offset(taipei_offset())
    }

    /// Generate timetable parent chains for every service in the active transit
    /// network when the request enables the top-level Transit mode.
    ///
    /// `access` contains the time at which a rider can reach each Boarding Point.
    /// The caller supplies that access through the street-routing boundary, keeping
    /// Walking and bicycle paths out of the timetable kernel.
    pub fn itineraries(
        &self,
        access: &[TransitAccess],
        query: TransitQuery,
        selected_modes: &[JourneyMode],
    ) -> Vec<TransitItinerary> {
        if access.is_empty() {
            return Vec::new();
        }
        let service_day = match query {
            TransitQuery::DepartAfter { earliest_departure } => {
                self.service_day(earliest_departure)
            }
            TransitQuery::ArriveBy { latest_arrival } => self.service_day(latest_arrival),
        };
        let indexes = self.indexes();
        let selected_service_indexes = self
            .services
            .iter()
            .enumerate()
            .filter(|_| selected_modes.contains(&JourneyMode::Transit))
            .map(|(index, _)| index)
            .collect::<HashSet<_>>();

        let mut frontier = access
            .iter()
            .map(|access| TransitLabel {
                stop_id: access.stop_id.clone(),
                available_at: access.available_at,
                arrival_at: access.available_at,
                access_departure_at: access.departure_at,
                rides: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut results = Vec::new();

        for _round in 0..=DEFAULT_MAX_TRANSFERS {
            let mut next = Vec::new();
            for label in &frontier {
                let earliest_seconds = seconds_since_service_day(
                    self.service_day_start(service_day),
                    label.available_at,
                );
                let mut services_seen = HashSet::new();
                for departure in indexes
                    .departures_by_stop
                    .get(&label.stop_id)
                    .into_iter()
                    .flatten()
                    .skip_while(|departure| departure.departure_seconds < earliest_seconds)
                {
                    let trip = &self.trips[departure.trip_index];
                    if !trip.runs_on(service_day) {
                        continue;
                    }
                    if !selected_service_indexes.contains(&departure.service_index)
                        || !services_seen.insert(departure.service_index)
                    {
                        continue;
                    }
                    let service = &self.services[departure.service_index];
                    for alight_index in (departure.stop_index + 1)..service.pattern.len() {
                        let departure_at = self.service_day_start(service_day)
                            + time::Duration::seconds(i64::from(
                                trip.stop_times[departure.stop_index].departure_seconds,
                            ));
                        let arrival_at = self.service_day_start(service_day)
                            + time::Duration::seconds(i64::from(
                                trip.stop_times[alight_index].arrival_seconds,
                            ));
                        let ride = TransitRide {
                            transit_mode: service.transit_mode,
                            service_id: service.id.clone(),
                            trip_id: trip.id.clone(),
                            board_stop_id: service.pattern[departure.stop_index].clone(),
                            alight_stop_id: service.pattern[alight_index].clone(),
                            scheduled_departure: departure_at,
                            scheduled_arrival: arrival_at,
                        };
                        let mut rides = label.rides.clone();
                        rides.push(ride);
                        let candidate = TransitLabel {
                            stop_id: service.pattern[alight_index].clone(),
                            available_at: arrival_at,
                            arrival_at,
                            access_departure_at: label.access_departure_at,
                            rides,
                        };
                        if candidate.rides.len() <= DEFAULT_MAX_TRANSFERS + 1 {
                            results_or_next(query, service_day, candidate, &mut results, &mut next);
                        }
                    }
                }
            }
            let mut transferred = Vec::new();
            for label in next {
                transferred.push(label.clone());
                for edge in indexes
                    .transfers_by_from
                    .get(&label.stop_id)
                    .into_iter()
                    .flatten()
                {
                    let Some(transfer_arrival) = label
                        .available_at
                        .checked_add(time::Duration::seconds(i64::from(edge.duration_seconds)))
                    else {
                        continue;
                    };
                    let mut transferred_label = label.clone();
                    transferred_label.stop_id = edge.to.clone();
                    transferred_label.available_at = transfer_arrival;
                    transferred.push(transferred_label);
                }
            }
            next = dominance_prune(transferred);
            next.truncate(128);
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        results.sort_by(|left, right| {
            left.arrival_at
                .cmp(&right.arrival_at)
                .then(left.transfers().cmp(&right.transfers()))
                .then(ride_signature(&left.rides).cmp(&ride_signature(&right.rides)))
        });
        results.dedup_by(|left, right| {
            left.rides == right.rides && left.access_departure_at == right.access_departure_at
        });
        results.truncate(64);
        results
    }

    fn indexes(&self) -> &TransitIndexes {
        self.indexes.get_or_init(|| TransitIndexes::build(self))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TransitLabel {
    stop_id: BoardingPointId,
    available_at: OffsetDateTime,
    arrival_at: OffsetDateTime,
    access_departure_at: OffsetDateTime,
    rides: Vec<TransitRide>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DepartureIndexEntry {
    trip_index: usize,
    service_index: usize,
    stop_index: usize,
    departure_seconds: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TransitIndexes {
    service_index_by_id: HashMap<TransitServiceId, usize>,
    trips_by_service: HashMap<usize, Vec<usize>>,
    departures_by_stop: HashMap<BoardingPointId, Vec<DepartureIndexEntry>>,
    transfers_by_from: HashMap<BoardingPointId, Vec<TransferEdge>>,
    stop_grid: SpatialGrid,
    station_grid: SpatialGrid,
}

impl TransitIndexes {
    fn build(network: &TransitNetwork) -> Self {
        let mut indexes = Self::default();
        for (index, stop) in network.stops.iter().enumerate() {
            indexes.stop_grid.insert(stop.coordinate, index);
        }
        for (index, station) in network.bike_share_stations.iter().enumerate() {
            indexes.station_grid.insert(station.coordinate, index);
        }
        for (service_index, service) in network.services.iter().enumerate() {
            indexes
                .service_index_by_id
                .insert(service.id.clone(), service_index);
        }
        for (trip_index, trip) in network.trips.iter().enumerate() {
            let Some(&service_index) = indexes.service_index_by_id.get(&trip.service_id) else {
                continue;
            };
            indexes
                .trips_by_service
                .entry(service_index)
                .or_default()
                .push(trip_index);
            let service = &network.services[service_index];
            if service.pattern.len() != trip.stop_times.len() {
                continue;
            }
            for (stop_index, stop_id) in service.pattern.iter().enumerate() {
                indexes
                    .departures_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(DepartureIndexEntry {
                        trip_index,
                        service_index,
                        stop_index,
                        departure_seconds: trip.stop_times[stop_index].departure_seconds,
                    });
            }
        }
        for edge in &network.transfer_edges {
            indexes
                .transfers_by_from
                .entry(edge.from.clone())
                .or_default()
                .push(edge.clone());
        }
        for edges in indexes.transfers_by_from.values_mut() {
            edges.sort_by(|left, right| {
                left.to
                    .as_str()
                    .cmp(right.to.as_str())
                    .then(left.duration_seconds.cmp(&right.duration_seconds))
            });
        }
        for entries in indexes.departures_by_stop.values_mut() {
            entries.sort_by(|left, right| {
                left.departure_seconds
                    .cmp(&right.departure_seconds)
                    .then(left.service_index.cmp(&right.service_index))
                    .then(left.trip_index.cmp(&right.trip_index))
                    .then(left.stop_index.cmp(&right.stop_index))
            });
        }
        indexes
    }
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
    fn insert(&mut self, coordinate: RequestCoordinate, index: usize) {
        self.cells
            .entry(SpatialCell::from_coordinate(coordinate))
            .or_default()
            .push(index);
    }
}

impl SpatialCell {
    fn from_coordinate(coordinate: RequestCoordinate) -> Self {
        Self {
            latitude: (coordinate.latitude / SPATIAL_CELL_DEGREES).floor() as i32,
            longitude: (coordinate.longitude / SPATIAL_CELL_DEGREES).floor() as i32,
        }
    }
}

trait SpatialPoint {
    fn coordinate(&self) -> RequestCoordinate;
    fn stable_id(&self) -> &str;
}

impl SpatialPoint for TransitStop {
    fn coordinate(&self) -> RequestCoordinate {
        self.coordinate
    }

    fn stable_id(&self) -> &str {
        self.id.as_str()
    }
}

impl SpatialPoint for BikeShareStation {
    fn coordinate(&self) -> RequestCoordinate {
        self.coordinate
    }

    fn stable_id(&self) -> &str {
        self.id.as_str()
    }
}

fn nearest_from_grid<'a, T: SpatialPoint>(
    grid: &SpatialGrid,
    points: &'a [T],
    coordinate: RequestCoordinate,
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
                .map(|point| distance_metres(coordinate, point.coordinate()))
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
        distance_metres(coordinate, left.coordinate())
            .cmp(&distance_metres(coordinate, right.coordinate()))
            .then(left.stable_id().cmp(right.stable_id()))
    });
    nearest.truncate(limit);
    nearest
}

fn distance_metres(from: RequestCoordinate, to: RequestCoordinate) -> u64 {
    let latitude = ((from.latitude + to.latitude) / 2.0).to_radians();
    let north = (to.latitude - from.latitude) * 111_320.0;
    let east = (to.longitude - from.longitude) * 111_320.0 * latitude.cos();
    north.hypot(east).max(0.0).round() as u64
}

fn cell_lower_bound_metres(coordinate: RequestCoordinate, cell: SpatialCell) -> u64 {
    let latitude_min = f64::from(cell.latitude) * SPATIAL_CELL_DEGREES;
    let latitude_max = latitude_min + SPATIAL_CELL_DEGREES;
    let longitude_min = f64::from(cell.longitude) * SPATIAL_CELL_DEGREES;
    let longitude_max = longitude_min + SPATIAL_CELL_DEGREES;
    let latitude = coordinate.latitude.clamp(latitude_min, latitude_max);
    let longitude = coordinate.longitude.clamp(longitude_min, longitude_max);
    distance_metres(
        coordinate,
        RequestCoordinate {
            latitude,
            longitude,
        },
    )
}

fn results_or_next(
    query: TransitQuery,
    service_day: Date,
    candidate: TransitLabel,
    results: &mut Vec<TransitItinerary>,
    next: &mut Vec<TransitLabel>,
) {
    let arrival_allowed = match query {
        TransitQuery::DepartAfter { .. } => true,
        TransitQuery::ArriveBy { latest_arrival } => candidate.arrival_at <= latest_arrival,
    };
    if arrival_allowed {
        results.push(TransitItinerary {
            service_day,
            access_departure_at: candidate.access_departure_at,
            arrival_at: candidate.arrival_at,
            rides: candidate.rides.clone(),
        });
    }
    if candidate.rides.len() <= DEFAULT_MAX_TRANSFERS {
        next.push(candidate);
    }
}

fn dominance_prune(mut labels: Vec<TransitLabel>) -> Vec<TransitLabel> {
    labels.sort_by(|left, right| {
        left.stop_id
            .as_str()
            .cmp(right.stop_id.as_str())
            .then(left.available_at.cmp(&right.available_at))
            .then(left.rides.len().cmp(&right.rides.len()))
            .then(ride_signature(&left.rides).cmp(&ride_signature(&right.rides)))
    });
    let mut retained = Vec::new();
    for label in labels {
        if retained.iter().any(|existing: &TransitLabel| {
            existing.stop_id == label.stop_id
                && existing.available_at <= label.available_at
                && existing.rides.len() <= label.rides.len()
        }) {
            continue;
        }
        retained.retain(|existing: &TransitLabel| {
            !(existing.stop_id == label.stop_id
                && label.available_at <= existing.available_at
                && label.rides.len() <= existing.rides.len())
        });
        retained.push(label);
    }
    retained
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitNetworkValidationError {
    #[error("transit artifact revisions are required")]
    MissingRevision,
    #[error("transit artifact must contain at least one stop")]
    EmptyStops,
    #[error("duplicate Boarding Point {0:?}")]
    DuplicateStop(BoardingPointId),
    #[error("invalid coordinate for Boarding Point {0:?}")]
    InvalidStopCoordinate(BoardingPointId),
    #[error("duplicate Transit Service {0:?}")]
    DuplicateService(TransitServiceId),
    #[error("duplicate Scheduled Trip {0:?}")]
    DuplicateTrip(ScheduledRunId),
    #[error("duplicate bike-share station {0:?}")]
    DuplicateStation(BikeShareStationId),
    #[error("invalid coordinate for bike-share station {0:?}")]
    InvalidStationCoordinate(BikeShareStationId),
    #[error("Transit Service {0:?} has fewer than two Boarding Points")]
    PatternTooShort(TransitServiceId),
    #[error("Transit Service {0:?} references an unknown Boarding Point")]
    UnknownPatternStop(TransitServiceId),
    #[error("Scheduled Trip references unknown Transit Service {0:?}")]
    UnknownTripService(TransitServiceId),
    #[error("Scheduled Trip {0:?} has an invalid shape or calendar")]
    InvalidTripShape(ScheduledRunId),
    #[error("Scheduled Trip {0:?} is not chronological")]
    NonChronologicalTrip(ScheduledRunId),
    #[error("transfer edge references unknown endpoints {from:?} -> {to:?}")]
    UnknownTransferEndpoint {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitArtifactError {
    #[error("transit artifact JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("transit artifact failed validation: {0}")]
    Invalid(TransitNetworkValidationError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitAccess {
    pub stop_id: BoardingPointId,
    pub departure_at: OffsetDateTime,
    pub available_at: OffsetDateTime,
}

impl TransitAccess {
    pub fn new(
        stop_id: BoardingPointId,
        departure_at: OffsetDateTime,
        available_at: OffsetDateTime,
    ) -> Self {
        Self {
            stop_id,
            departure_at,
            available_at,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitQuery {
    DepartAfter { earliest_departure: OffsetDateTime },
    ArriveBy { latest_arrival: OffsetDateTime },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitRide {
    pub transit_mode: TransitMode,
    pub service_id: TransitServiceId,
    pub trip_id: ScheduledRunId,
    pub board_stop_id: BoardingPointId,
    pub alight_stop_id: BoardingPointId,
    pub scheduled_departure: OffsetDateTime,
    pub scheduled_arrival: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitItinerary {
    pub service_day: Date,
    pub access_departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub rides: Vec<TransitRide>,
}

impl TransitItinerary {
    pub fn transfers(&self) -> usize {
        self.rides.len().saturating_sub(1)
    }
}

fn seconds_since_service_day(start: OffsetDateTime, instant: OffsetDateTime) -> u32 {
    let seconds = (normalize_to_taipei(instant) - start).whole_seconds();
    u32::try_from(seconds.max(0)).unwrap_or(u32::MAX)
}

fn ride_signature(rides: &[TransitRide]) -> String {
    rides
        .iter()
        .map(|ride| format!("{}:{}", ride.service_id.as_str(), ride.trip_id.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}
