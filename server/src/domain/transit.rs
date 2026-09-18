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

use super::ids::{BikeShareStationId, BoardingPointId, ScheduledTripId, TransitServiceId};
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

/// A static walking connection between a public-transit Boarding Point and a
/// BikeShare station. The two directions are intentionally independent: the
/// street graph may make one direction unreachable or materially different.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BikeShareConnector {
    pub stop_id: BoardingPointId,
    pub station_id: BikeShareStationId,
    #[serde(default)]
    pub stop_to_station_seconds: Option<u32>,
    #[serde(default)]
    pub station_to_stop_seconds: Option<u32>,
    #[serde(default)]
    pub stop_to_station_metres: Option<u32>,
    #[serde(default)]
    pub station_to_stop_metres: Option<u32>,
}

impl BikeShareConnector {
    pub fn new(stop_id: BoardingPointId, station_id: BikeShareStationId) -> Self {
        Self {
            stop_id,
            station_id,
            stop_to_station_seconds: None,
            station_to_stop_seconds: None,
            stop_to_station_metres: None,
            station_to_stop_metres: None,
        }
    }

    pub fn with_stop_to_station(mut self, seconds: u32, metres: u32) -> Self {
        self.stop_to_station_seconds = Some(seconds);
        self.stop_to_station_metres = Some(metres);
        self
    }

    pub fn with_station_to_stop(mut self, seconds: u32, metres: u32) -> Self {
        self.station_to_stop_seconds = Some(seconds);
        self.station_to_stop_metres = Some(metres);
        self
    }

    pub fn has_direction(&self) -> bool {
        self.stop_to_station_seconds.is_some() || self.station_to_stop_seconds.is_some()
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
    pub bike_share_stations: Vec<BikeShareStation>,
    /// Explicit movement time between distinct Boarding Points. Same-point
    /// transfers use `default_transfer_seconds`; distinct points must be listed.
    #[serde(default)]
    pub transfer_edges: Vec<TransferEdge>,
    /// Static walking connectors between transit Boarding Points and distinct
    /// BikeShare stations. Realtime availability is never persisted here.
    #[serde(default)]
    pub bike_share_connectors: Vec<BikeShareConnector>,
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
            && self.bike_share_stations == other.bike_share_stations
            && self.transfer_edges == other.transfer_edges
            && self.bike_share_connectors == other.bike_share_connectors
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
            metro_timings: Vec::new(),
            bike_share_stations,
            transfer_edges: Vec::new(),
            bike_share_connectors: Vec::new(),
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

    pub fn with_bike_share_connectors(
        mut self,
        bike_share_connectors: Vec<BikeShareConnector>,
    ) -> Self {
        self.bike_share_connectors = bike_share_connectors;
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
        let mut connector_pairs = HashSet::new();
        for connector in &self.bike_share_connectors {
            if !stop_ids.contains(&connector.stop_id) {
                return Err(TransitNetworkValidationError::UnknownBikeConnectorStop(
                    connector.stop_id.clone(),
                ));
            }
            if !station_ids.contains(&connector.station_id) {
                return Err(TransitNetworkValidationError::UnknownBikeConnectorStation(
                    connector.station_id.clone(),
                ));
            }
            if !connector_pairs.insert((connector.stop_id.clone(), connector.station_id.clone())) {
                return Err(TransitNetworkValidationError::DuplicateBikeConnector {
                    stop_id: connector.stop_id.clone(),
                    station_id: connector.station_id.clone(),
                });
            }
            if !connector.has_direction() {
                return Err(TransitNetworkValidationError::EmptyBikeConnector {
                    stop_id: connector.stop_id.clone(),
                    station_id: connector.station_id.clone(),
                });
            }
            if connector.stop_to_station_seconds == Some(0)
                || connector.station_to_stop_seconds == Some(0)
            {
                return Err(TransitNetworkValidationError::ZeroBikeConnectorDuration {
                    stop_id: connector.stop_id.clone(),
                    station_id: connector.station_id.clone(),
                });
            }
            if connector.stop_to_station_seconds.is_some()
                != connector.stop_to_station_metres.is_some()
                || connector.station_to_stop_seconds.is_some()
                    != connector.station_to_stop_metres.is_some()
            {
                return Err(TransitNetworkValidationError::InvalidBikeConnectorMetrics {
                    stop_id: connector.stop_id.clone(),
                    station_id: connector.station_id.clone(),
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
        self.bike_share_stations
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.transfer_edges.sort_by(|left, right| {
            left.from
                .as_str()
                .cmp(right.from.as_str())
                .then(left.to.as_str().cmp(right.to.as_str()))
                .then(left.duration_seconds.cmp(&right.duration_seconds))
        });
        self.transfer_edges
            .dedup_by(|left, right| left.from == right.from && left.to == right.to);
        self.bike_share_connectors.sort_by(|left, right| {
            left.stop_id
                .as_str()
                .cmp(right.stop_id.as_str())
                .then(left.station_id.as_str().cmp(right.station_id.as_str()))
        });
        self.bike_share_connectors.dedup_by(|left, right| {
            left.stop_id == right.stop_id && left.station_id == right.station_id
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

    /// Return all bike-share stations inside a bounded radius in deterministic
    /// distance/id order.
    pub fn bike_share_stations_within_radius(
        &self,
        coordinate: RequestCoordinate,
        radius_metres: u32,
    ) -> Vec<&BikeShareStation> {
        points_within_radius_from_grid(
            &self.indexes().station_grid,
            &self.bike_share_stations,
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
        self.services.iter().find(|service| &service.id == id)
    }

    pub fn bike_share_station(&self, id: &BikeShareStationId) -> Option<&BikeShareStation> {
        self.bike_share_stations
            .iter()
            .find(|station| &station.id == id)
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

        let shape = self
            .trips
            .iter()
            .filter(|trip| &trip.service_id == service_id)
            .find_map(|trip| {
                let board = trip.stop_times.get(board_index)?;
                let alight = trip.stop_times.get(alight_index)?;
                Some((board.departure_seconds, alight.arrival_seconds))
            });
        let (departure_seconds, arrival_seconds) = shape.unwrap_or_else(|| {
            let duration = self.estimated_travel_seconds(service, board_index, alight_index);
            (0, duration)
        });
        arrival_seconds
            .checked_sub(departure_seconds)
            .filter(|duration| *duration > 0)
    }

    fn estimated_travel_seconds(
        &self,
        service: &TransitService,
        board_index: usize,
        alight_index: usize,
    ) -> u32 {
        self.trips
            .iter()
            .filter(|trip| trip.service_id == service.id)
            .filter_map(|trip| {
                let board = trip.stop_times.get(board_index)?;
                let alight = trip.stop_times.get(alight_index)?;
                Some(
                    alight
                        .arrival_seconds
                        .saturating_sub(board.departure_seconds),
                )
            })
            .find(|duration| *duration > 0)
            .unwrap_or_else(|| {
                u32::try_from(alight_index.saturating_sub(board_index))
                    .unwrap_or(u32::MAX)
                    .saturating_mul(120)
            })
    }

    fn indexes(&self) -> &TransitIndexes {
        self.indexes.get_or_init(|| TransitIndexes::build(self))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TransitIndexes {
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
    #[error("BikeShare connector references unknown Boarding Point {0:?}")]
    UnknownBikeConnectorStop(BoardingPointId),
    #[error("BikeShare connector references unknown station {0:?}")]
    UnknownBikeConnectorStation(BikeShareStationId),
    #[error("duplicate BikeShare connector {stop_id:?} -> {station_id:?}")]
    DuplicateBikeConnector {
        stop_id: BoardingPointId,
        station_id: BikeShareStationId,
    },
    #[error("BikeShare connector has no reachable direction {stop_id:?} <-> {station_id:?}")]
    EmptyBikeConnector {
        stop_id: BoardingPointId,
        station_id: BikeShareStationId,
    },
    #[error("BikeShare connector has incomplete metrics {stop_id:?} <-> {station_id:?}")]
    InvalidBikeConnectorMetrics {
        stop_id: BoardingPointId,
        station_id: BikeShareStationId,
    },
    #[error("BikeShare connector has zero duration {stop_id:?} <-> {station_id:?}")]
    ZeroBikeConnectorDuration {
        stop_id: BoardingPointId,
        station_id: BikeShareStationId,
    },
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitArtifactError {
    #[error("transit artifact JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("transit artifact failed validation: {0}")]
    Invalid(TransitNetworkValidationError),
}
