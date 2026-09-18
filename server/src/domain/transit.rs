//! Immutable, provider-neutral transit data consumed by the routing layer.
//!
//! Static adapters build validated `TransitNetwork` artifacts. Requests read one
//! immutable revision and resolve departures through the timing boundary.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use time::{Date, OffsetDateTime, PrimitiveDateTime, Time, Weekday};

use super::ids::{BoardingPointId, ScheduledTripId, TransitServiceId};
use super::{
    location::{METRES_PER_DEGREE, RequestCoordinate},
    normalize_to_taipei, taipei_offset,
};

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
    /// Opaque adapter key used by timing composition. It is intentionally not
    /// an endpoint name or a mode-derived routing rule.
    #[serde(default)]
    pub timing_source: Option<String>,
    /// Provider-neutral partition metadata used when resolving live arrivals.
    #[serde(default)]
    pub realtime_scope: Option<String>,
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
            timing_source: None,
            realtime_scope: None,
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

    pub fn with_timing_source(mut self, source: impl Into<String>) -> Self {
        self.timing_source = Some(source.into());
        self
    }

    pub fn with_realtime_scope(mut self, scope: impl Into<String>) -> Self {
        self.realtime_scope = Some(scope.into());
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
    pub id: ScheduledTripId,
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
        id: ScheduledTripId,
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

    pub(crate) fn runs_on(&self, service_day: Date) -> bool {
        if self.removed_service_days.contains(&service_day) {
            return false;
        }
        self.added_service_days.contains(&service_day)
            || self.active_weekdays.contains(&service_day.weekday())
    }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetroFrequency {
    /// Seconds from the beginning of the service day.
    pub start_seconds: u32,
    /// Seconds from the beginning of the service day.
    pub end_seconds: u32,
    pub headway_seconds: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetroStationTiming {
    pub stop_id: BoardingPointId,
    #[serde(default)]
    pub departures_seconds: Vec<u32>,
    #[serde(default)]
    pub frequencies: Vec<MetroFrequency>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetroTiming {
    pub service_id: TransitServiceId,
    pub station_timings: Vec<MetroStationTiming>,
    /// One positive duration for every adjacent pair in the service pattern.
    #[serde(default)]
    pub inter_station_travel_seconds: Vec<u32>,
    /// The ordered pattern used when the timing was normalized. Older
    /// artifacts may omit this field; newly built artifacts use it to keep
    /// directional timing from being attached to the opposite service.
    #[serde(default)]
    pub pattern: Vec<BoardingPointId>,
}

impl MetroTiming {
    pub fn new(
        service_id: TransitServiceId,
        station_timings: Vec<MetroStationTiming>,
        inter_station_travel_seconds: Vec<u32>,
    ) -> Self {
        Self {
            service_id,
            station_timings,
            inter_station_travel_seconds,
            pattern: Vec::new(),
        }
    }

    pub fn with_pattern(mut self, pattern: Vec<BoardingPointId>) -> Self {
        self.pattern = pattern;
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitNetwork {
    pub service_day_boundary: Time,
    pub stops: Vec<TransitStop>,
    pub services: Vec<TransitService>,
    pub trips: Vec<ScheduledTrip>,
    #[serde(default)]
    pub metro_timings: Vec<MetroTiming>,
    /// Explicit movement time between distinct Boarding Points. Same-point
    /// transfers use `default_transfer_seconds`; distinct points must be listed.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct NetworkRevision {
    pub static_network_revision: String,
    pub schedule_revision: String,
}

impl PartialEq for TransitNetwork {
    fn eq(&self, other: &Self) -> bool {
        self.service_day_boundary == other.service_day_boundary
            && self.stops == other.stops
            && self.services == other.services
            && self.trips == other.trips
            && self.metro_timings == other.metro_timings
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
        )
    }
}

impl TransitNetwork {
    pub fn new(
        service_day_boundary: Time,
        stops: Vec<TransitStop>,
        services: Vec<TransitService>,
        trips: Vec<ScheduledTrip>,
    ) -> Self {
        Self {
            service_day_boundary,
            stops,
            services,
            trips,
            metro_timings: Vec::new(),
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

    pub fn with_metro_timings(mut self, metro_timings: Vec<MetroTiming>) -> Self {
        self.metro_timings = metro_timings;
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
        let mut metro_timing_services = HashSet::new();
        for timing in &self.metro_timings {
            if !metro_timing_services.insert(timing.service_id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateMetroTiming(
                    timing.service_id.clone(),
                ));
            }
            let Some(service) = self.service(&timing.service_id) else {
                return Err(TransitNetworkValidationError::UnknownMetroTimingService(
                    timing.service_id.clone(),
                ));
            };
            if service.transit_mode != TransitMode::Metro
                || timing.inter_station_travel_seconds.len() != service.pattern.len() - 1
                || timing.inter_station_travel_seconds.contains(&0)
                || (!timing.pattern.is_empty() && timing.pattern != service.pattern)
            {
                return Err(TransitNetworkValidationError::InvalidMetroTimingShape(
                    timing.service_id.clone(),
                ));
            }
            for station_timing in &timing.station_timings {
                if !service.pattern.contains(&station_timing.stop_id)
                    || station_timing
                        .departures_seconds
                        .windows(2)
                        .any(|times| times[0] >= times[1])
                    || station_timing.frequencies.iter().any(|frequency| {
                        frequency.start_seconds > frequency.end_seconds
                            || frequency.headway_seconds == 0
                    })
                {
                    return Err(TransitNetworkValidationError::InvalidMetroTimingShape(
                        timing.service_id.clone(),
                    ));
                }
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
        for edge in &self.transfer_edges {
            if !stop_ids.contains(&edge.from) || !stop_ids.contains(&edge.to) {
                return Err(TransitNetworkValidationError::UnknownTransferEndpoint {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                });
            }
            if edge.from == edge.to {
                return Err(TransitNetworkValidationError::SelfTransfer(
                    edge.from.clone(),
                ));
            }
            if edge.duration_seconds == 0 {
                return Err(TransitNetworkValidationError::ZeroTransferDuration {
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
        self.metro_timings
            .sort_by(|left, right| left.service_id.as_str().cmp(right.service_id.as_str()));
        for timing in &mut self.metro_timings {
            timing
                .station_timings
                .sort_by(|left, right| left.stop_id.as_str().cmp(right.stop_id.as_str()));
            for station_timing in &mut timing.station_timings {
                station_timing.departures_seconds.sort_unstable();
                station_timing.frequencies.sort_by(|left, right| {
                    left.start_seconds
                        .cmp(&right.start_seconds)
                        .then(left.end_seconds.cmp(&right.end_seconds))
                        .then(left.headway_seconds.cmp(&right.headway_seconds))
                });
            }
        }
        self.transfer_edges.sort_by(|left, right| {
            left.from
                .as_str()
                .cmp(right.from.as_str())
                .then(left.to.as_str().cmp(right.to.as_str()))
                .then(left.duration_seconds.cmp(&right.duration_seconds))
        });
        self.transfer_edges
            .dedup_by(|left, right| left.from == right.from && left.to == right.to);
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

    pub fn revision(&self) -> NetworkRevision {
        NetworkRevision {
            static_network_revision: self.static_network_revision.clone(),
            schedule_revision: self.schedule_revision.clone(),
        }
    }

    /// Return spatially nearest stops in deterministic distance/id order.
    /// The grid avoids scanning all of Taipei for the normal bounded query;
    /// the bounded fallback preserves correctness for sparse fixtures.
    pub fn nearest_stops(&self, coordinate: RequestCoordinate, limit: usize) -> Vec<&TransitStop> {
        nearest_from_grid(&self.indexes().stop_grid, &self.stops, coordinate, limit)
    }

    /// Return all stops inside a bounded radius in deterministic distance/id order.
    /// The query uses the same immutable grid as nearest-stop lookup, so callers
    /// do not need to maintain a second spatial index over the network.
    pub fn stops_within_radius(
        &self,
        coordinate: RequestCoordinate,
        radius_metres: u32,
    ) -> Vec<&TransitStop> {
        points_within_radius_from_grid(
            &self.indexes().stop_grid,
            &self.stops,
            coordinate,
            radius_metres,
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
        let index = self.indexes().service_by_id.get(id).copied()?;
        self.services.get(index)
    }

    pub fn services_at_stop(&self, stop_id: &BoardingPointId) -> &[TransitServiceId] {
        self.indexes()
            .services_by_stop
            .get(stop_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn modes_at_stop(&self, stop_id: &BoardingPointId) -> &[TransitMode] {
        self.indexes()
            .modes_by_stop
            .get(stop_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn scheduled_trip(&self, id: &ScheduledTripId) -> Option<&ScheduledTrip> {
        let index = self.indexes().scheduled_trip_by_id.get(id).copied()?;
        self.trips.get(index)
    }

    pub(crate) fn scheduled_trip_at(&self, index: usize) -> Option<&ScheduledTrip> {
        self.trips.get(index)
    }

    pub(crate) fn scheduled_trip_indices(
        &self,
        service_id: &TransitServiceId,
        board_index: usize,
    ) -> &[usize] {
        self.indexes()
            .scheduled_trip_indices_by_service_stop
            .get(service_id)
            .and_then(|by_stop| by_stop.get(board_index))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn representative_trip(
        &self,
        service_id: &TransitServiceId,
    ) -> Option<&ScheduledTrip> {
        let index = self
            .indexes()
            .representative_trip_by_service
            .get(service_id)
            .copied()?;
        self.trips.get(index)
    }

    pub(crate) fn has_invalid_timing_shape(&self, service_id: &TransitServiceId) -> bool {
        self.indexes()
            .invalid_timing_shape_by_service
            .contains(service_id)
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

    /// Return a representative duration for one adjacent service segment.
    ///
    /// Realtime bus departures may not have a scheduled trip for the current
    /// service day, so the timing adapter needs a static shape without
    /// fabricating a departure identity.
    pub fn estimated_segment_seconds(
        &self,
        service_id: &TransitServiceId,
        from_stop_id: &BoardingPointId,
        to_stop_id: &BoardingPointId,
    ) -> Option<u32> {
        let service = self.service(service_id)?;
        let board_index = service.pattern.iter().position(|id| id == from_stop_id)?;
        let alight_index = service
            .pattern
            .iter()
            .enumerate()
            .skip(board_index + 1)
            .find_map(|(index, id)| (id == to_stop_id).then_some(index))?;

        let durations = self
            .indexes()
            .segment_durations_by_service
            .get(service_id)?;
        let duration = durations
            .get(board_index..alight_index)?
            .iter()
            .try_fold(0_u32, |total, duration| total.checked_add(*duration))?;
        (duration > 0).then_some(duration)
    }

    fn indexes(&self) -> &TransitIndexes {
        self.indexes.get_or_init(|| TransitIndexes::build(self))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TransitIndexes {
    stop_grid: SpatialGrid,
    service_by_id: HashMap<TransitServiceId, usize>,
    services_by_stop: HashMap<BoardingPointId, Vec<TransitServiceId>>,
    modes_by_stop: HashMap<BoardingPointId, Vec<TransitMode>>,
    scheduled_trip_indices_by_service_stop: HashMap<TransitServiceId, Vec<Vec<usize>>>,
    scheduled_trip_by_id: HashMap<ScheduledTripId, usize>,
    representative_trip_by_service: HashMap<TransitServiceId, usize>,
    segment_durations_by_service: HashMap<TransitServiceId, Vec<u32>>,
    invalid_timing_shape_by_service: HashSet<TransitServiceId>,
}

impl TransitIndexes {
    fn build(network: &TransitNetwork) -> Self {
        let mut indexes = Self::default();
        for (index, stop) in network.stops.iter().enumerate() {
            indexes.stop_grid.insert(stop.coordinate, index);
        }
        for (index, service) in network.services.iter().enumerate() {
            indexes.service_by_id.insert(service.id.clone(), index);
            for stop_id in &service.pattern {
                indexes
                    .services_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(service.id.clone());
                indexes
                    .modes_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(service.transit_mode);
            }
        }
        for service_ids in indexes.services_by_stop.values_mut() {
            service_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            service_ids.dedup();
        }
        for modes in indexes.modes_by_stop.values_mut() {
            modes.sort_by_key(|mode| *mode as u8);
            modes.dedup();
        }
        for (trip_index, trip) in network.trips.iter().enumerate() {
            indexes
                .scheduled_trip_by_id
                .insert(trip.id.clone(), trip_index);
            let Some(service_index) = indexes.service_by_id.get(&trip.service_id).copied() else {
                continue;
            };
            let service = &network.services[service_index];
            if trip.stop_times.len() != service.pattern.len() {
                continue;
            }
            let trip_indices = indexes
                .scheduled_trip_indices_by_service_stop
                .entry(trip.service_id.clone())
                .or_insert_with(|| vec![Vec::new(); service.pattern.len()]);
            for trip_indices_at_stop in trip_indices.iter_mut().take(trip.stop_times.len()) {
                trip_indices_at_stop.push(trip_index);
            }
            if trip.stop_times.windows(2).any(|times| {
                times[0].arrival_seconds > times[0].departure_seconds
                    || times[0].departure_seconds > times[1].arrival_seconds
            }) {
                indexes
                    .invalid_timing_shape_by_service
                    .insert(trip.service_id.clone());
                continue;
            }
            let replace_representative = indexes
                .representative_trip_by_service
                .get(&trip.service_id)
                .is_none_or(|current| network.trips[*current].id.as_str() > trip.id.as_str());
            if replace_representative {
                indexes
                    .representative_trip_by_service
                    .insert(trip.service_id.clone(), trip_index);
            }
        }
        for trip_indices_by_stop in indexes.scheduled_trip_indices_by_service_stop.values_mut() {
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
        for service in &network.services {
            let durations = indexes
                .representative_trip_by_service
                .get(&service.id)
                .map(|trip_index| {
                    let trip = &network.trips[*trip_index];
                    trip.stop_times
                        .windows(2)
                        .map(|times| {
                            times[1]
                                .arrival_seconds
                                .saturating_sub(times[0].departure_seconds)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec![120; service.pattern.len().saturating_sub(1)]);
            indexes
                .segment_durations_by_service
                .insert(service.id.clone(), durations);
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
    coordinate: RequestCoordinate,
    radius_metres: u32,
) -> Vec<&'a T> {
    if points.is_empty() {
        return Vec::new();
    }

    let latitude_delta = f64::from(radius_metres) / METRES_PER_DEGREE;
    let longitude_scale =
        METRES_PER_DEGREE * coordinate.latitude.to_radians().cos().abs().max(0.01);
    let longitude_delta = f64::from(radius_metres) / longitude_scale;
    let latitude_min = SpatialCell::from_coordinate(RequestCoordinate {
        latitude: coordinate.latitude - latitude_delta,
        longitude: coordinate.longitude,
    })
    .latitude;
    let latitude_max = SpatialCell::from_coordinate(RequestCoordinate {
        latitude: coordinate.latitude + latitude_delta,
        longitude: coordinate.longitude,
    })
    .latitude;
    let longitude_min = SpatialCell::from_coordinate(RequestCoordinate {
        latitude: coordinate.latitude,
        longitude: coordinate.longitude - longitude_delta,
    })
    .longitude;
    let longitude_max = SpatialCell::from_coordinate(RequestCoordinate {
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

fn cell_lower_bound_metres(coordinate: RequestCoordinate, cell: SpatialCell) -> u64 {
    let latitude_min = f64::from(cell.latitude) * SPATIAL_CELL_DEGREES;
    let latitude_max = latitude_min + SPATIAL_CELL_DEGREES;
    let longitude_min = f64::from(cell.longitude) * SPATIAL_CELL_DEGREES;
    let longitude_max = longitude_min + SPATIAL_CELL_DEGREES;
    let latitude = coordinate.latitude.clamp(latitude_min, latitude_max);
    let longitude = coordinate.longitude.clamp(longitude_min, longitude_max);
    coordinate.planar_distance_metres(RequestCoordinate {
        latitude,
        longitude,
    })
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
    DuplicateTrip(ScheduledTripId),
    #[error("Transit Service {0:?} has fewer than two Boarding Points")]
    PatternTooShort(TransitServiceId),
    #[error("Transit Service {0:?} references an unknown Boarding Point")]
    UnknownPatternStop(TransitServiceId),
    #[error("Scheduled Trip references unknown Transit Service {0:?}")]
    UnknownTripService(TransitServiceId),
    #[error("duplicate Metro timing for Transit Service {0:?}")]
    DuplicateMetroTiming(TransitServiceId),
    #[error("Metro timing references unknown Transit Service {0:?}")]
    UnknownMetroTimingService(TransitServiceId),
    #[error("Metro timing for Transit Service {0:?} has an invalid shape")]
    InvalidMetroTimingShape(TransitServiceId),
    #[error("Scheduled Trip {0:?} has an invalid shape or calendar")]
    InvalidTripShape(ScheduledTripId),
    #[error("Scheduled Trip {0:?} is not chronological")]
    NonChronologicalTrip(ScheduledTripId),
    #[error("transfer edge references unknown endpoints {from:?} -> {to:?}")]
    UnknownTransferEndpoint {
        from: BoardingPointId,
        to: BoardingPointId,
    },
    #[error("transfer edge is a self-transfer at {0:?}")]
    SelfTransfer(BoardingPointId),
    #[error("transfer edge has zero duration {from:?} -> {to:?}")]
    ZeroTransferDuration {
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
