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

use super::ids::{
    BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
};
use super::{
    location::{METRES_PER_DEGREE, RequestCoordinate},
    normalize_to_taipei, taipei_offset,
};

const SPATIAL_CELL_DEGREES: f64 = 0.005;
const MAX_SPATIAL_SEARCH_RING: i32 = 32;

pub fn default_service_day_boundary() -> Time {
    Time::from_hms(4, 0, 0).expect("service-day boundary is valid")
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitRoute {
    pub id: TransitRouteId,
    pub mode: TransitMode,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentTimeSource {
    Schedule,
    Derived,
    Estimated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentTravelTime {
    pub seconds: u32,
    pub source: SegmentTimeSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentProfile {
    pub segments: Vec<SegmentTravelTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceCalendar {
    pub id: ServiceCalendarId,
    pub weekdays: Vec<Weekday>,
    pub start_date: Option<Date>,
    pub end_date: Option<Date>,
    pub added_dates: Vec<Date>,
    pub removed_dates: Vec<Date>,
}

impl ServiceCalendar {
    pub fn runs_on(&self, service_day: Date) -> bool {
        calendar_runs_on(
            service_day,
            &self.weekdays,
            self.start_date,
            self.end_date,
            &self.added_dates,
            &self.removed_dates,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitPattern {
    pub id: TransitPatternId,
    pub route_id: TransitRouteId,
    pub direction: Option<u8>,
    pub headsign: String,
    pub stops: Vec<BoardingPointId>,
    pub static_profile: SegmentProfile,
}

impl TransitPattern {
    pub fn new(
        id: TransitPatternId,
        route_id: TransitRouteId,
        direction: Option<u8>,
        headsign: impl Into<String>,
        stops: Vec<BoardingPointId>,
        static_profile: SegmentProfile,
    ) -> Self {
        Self {
            id,
            route_id,
            direction,
            headsign: headsign.into(),
            stops,
            static_profile,
        }
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
pub struct TransitTrip {
    pub id: TransitTripId,
    pub pattern_id: TransitPatternId,
    pub calendar_id: ServiceCalendarId,
    pub stop_times: Vec<ScheduledStopTime>,
}

impl TransitTrip {
    pub fn new(
        id: TransitTripId,
        pattern_id: TransitPatternId,
        calendar_id: ServiceCalendarId,
        stop_times: Vec<ScheduledStopTime>,
    ) -> Self {
        Self {
            id,
            pattern_id,
            calendar_id,
            stop_times,
        }
    }
}

/// GTFS frequency-based service. `template_stop_times` retains the complete
/// GTFS trip template; it is not added to the scheduled-trip collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitFrequency {
    pub id: TransitTripId,
    pub pattern_id: TransitPatternId,
    pub calendar_id: ServiceCalendarId,
    pub start_seconds: u32,
    pub end_seconds: u32,
    pub headway_seconds: u32,
    pub exact_times: bool,
    pub template_stop_times: Vec<ScheduledStopTime>,
}

fn calendar_runs_on(
    service_day: Date,
    active_weekdays: &[Weekday],
    start_date: Option<Date>,
    end_date: Option<Date>,
    added_service_days: &[Date],
    removed_service_days: &[Date],
) -> bool {
    if removed_service_days.contains(&service_day) {
        return false;
    }
    if added_service_days.contains(&service_day) {
        return true;
    }
    if start_date.is_some_and(|start| service_day < start)
        || end_date.is_some_and(|end| service_day > end)
    {
        return false;
    }
    active_weekdays.contains(&service_day.weekday())
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitNetwork {
    pub service_day_boundary: Time,
    pub stops: Vec<TransitStop>,
    pub routes: Vec<TransitRoute>,
    pub patterns: Vec<TransitPattern>,
    pub calendars: Vec<ServiceCalendar>,
    pub trips: Vec<TransitTrip>,
    #[serde(default)]
    pub frequencies: Vec<TransitFrequency>,
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
            && self.routes == other.routes
            && self.patterns == other.patterns
            && self.calendars == other.calendars
            && self.trips == other.trips
            && self.frequencies == other.frequencies
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
        patterns: Vec<TransitPattern>,
        trips: Vec<TransitTrip>,
    ) -> Self {
        Self {
            service_day_boundary,
            stops,
            routes: Vec::new(),
            patterns,
            calendars: Vec::new(),
            trips,
            frequencies: Vec::new(),
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

    pub fn with_routes(mut self, routes: Vec<TransitRoute>) -> Self {
        self.routes = routes;
        self
    }

    pub fn with_calendars(mut self, calendars: Vec<ServiceCalendar>) -> Self {
        self.calendars = calendars;
        self
    }

    pub fn with_transfer_edges(mut self, transfer_edges: Vec<TransferEdge>) -> Self {
        self.transfer_edges = transfer_edges;
        self
    }

    pub fn with_frequencies(mut self, frequencies: Vec<TransitFrequency>) -> Self {
        self.frequencies = frequencies;
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
        let mut route_ids = HashSet::new();
        for route in &self.routes {
            if !route_ids.insert(route.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateRoute(
                    route.id.clone(),
                ));
            }
        }
        let mut service_ids = HashSet::new();
        let mut pattern_topologies = HashSet::new();
        for service in &self.patterns {
            if !service_ids.insert(service.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateService(
                    service.id.clone(),
                ));
            }
            if service.stops.len() < 2 {
                return Err(TransitNetworkValidationError::PatternTooShort(
                    service.id.clone(),
                ));
            }
            if !route_ids.contains(&service.route_id) {
                return Err(TransitNetworkValidationError::UnknownPatternRoute(
                    service.id.clone(),
                ));
            }
            if !pattern_topologies.insert((
                service.route_id.clone(),
                service.direction,
                service.stops.clone(),
            )) {
                return Err(TransitNetworkValidationError::DuplicatePatternTopology(
                    service.id.clone(),
                ));
            }
            if service.static_profile.segments.len() != service.stops.len() - 1
                || service
                    .static_profile
                    .segments
                    .iter()
                    .any(|segment| segment.seconds == 0)
            {
                return Err(TransitNetworkValidationError::InvalidPatternProfile(
                    service.id.clone(),
                ));
            }
            if service
                .stops
                .iter()
                .any(|stop_id| !stop_ids.contains(stop_id))
            {
                return Err(TransitNetworkValidationError::UnknownPatternStop(
                    service.id.clone(),
                ));
            }
        }
        let mut calendar_ids = HashSet::new();
        for calendar in &self.calendars {
            if !calendar_ids.insert(calendar.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicateCalendar(
                    calendar.id.clone(),
                ));
            }
            if (calendar.weekdays.is_empty() && calendar.added_dates.is_empty())
                || calendar
                    .start_date
                    .zip(calendar.end_date)
                    .is_some_and(|(start, end)| start > end)
            {
                return Err(TransitNetworkValidationError::InvalidCalendar(
                    calendar.id.clone(),
                ));
            }
        }
        let pattern_by_id = self
            .patterns
            .iter()
            .map(|pattern| (&pattern.id, pattern))
            .collect::<HashMap<_, _>>();
        for trip in &self.trips {
            let Some(service) = pattern_by_id.get(&trip.pattern_id).copied() else {
                return Err(TransitNetworkValidationError::UnknownTripService(
                    trip.pattern_id.clone(),
                ));
            };
            if !calendar_ids.contains(&trip.calendar_id)
                || trip.stop_times.len() != service.stops.len()
            {
                return Err(TransitNetworkValidationError::InvalidTripShape(
                    trip.id.clone(),
                ));
            }
            if trip
                .stop_times
                .last()
                .is_some_and(|last| last.arrival_seconds > last.departure_seconds)
                || trip.stop_times.windows(2).any(|times| {
                    times[0].arrival_seconds > times[0].departure_seconds
                        || times[0].departure_seconds > times[1].arrival_seconds
                })
            {
                return Err(TransitNetworkValidationError::NonChronologicalTrip(
                    trip.id.clone(),
                ));
            }
        }
        let mut frequency_ids = HashSet::new();
        for frequency in &self.frequencies {
            let Some(service) = pattern_by_id.get(&frequency.pattern_id).copied() else {
                return Err(TransitNetworkValidationError::UnknownFrequencyService(
                    frequency.pattern_id.clone(),
                ));
            };
            if !frequency_ids.insert(frequency.id.clone())
                || frequency.start_seconds >= frequency.end_seconds
                || frequency.headway_seconds == 0
                || frequency.template_stop_times.len() != service.stops.len()
                || !calendar_ids.contains(&frequency.calendar_id)
                || frequency
                    .template_stop_times
                    .last()
                    .is_some_and(|last| last.arrival_seconds > last.departure_seconds)
                || frequency.template_stop_times.windows(2).any(|times| {
                    times[0].arrival_seconds > times[0].departure_seconds
                        || times[0].departure_seconds > times[1].arrival_seconds
                })
            {
                return Err(TransitNetworkValidationError::InvalidFrequencyShape(
                    frequency.id.clone(),
                ));
            }
        }
        let mut trip_ids = HashSet::new();
        for trip in &self.trips {
            if !trip_ids.insert(trip.id.clone()) || frequency_ids.contains(&trip.id) {
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
        self.routes
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.calendars
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.patterns
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.trips
            .sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        self.frequencies.sort_by(|left, right| {
            left.pattern_id
                .as_str()
                .cmp(right.pattern_id.as_str())
                .then(left.start_seconds.cmp(&right.start_seconds))
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
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
        nearest_from_grid(
            &self.indexes().spatial.stop_grid,
            &self.stops,
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
            &self.indexes().spatial.stop_grid,
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
        let index = self.indexes().lookup.stop_by_id.get(id).copied()?;
        self.stops.get(index)
    }

    pub fn pattern(&self, id: &TransitPatternId) -> Option<&TransitPattern> {
        let index = self.indexes().lookup.pattern_by_id.get(id).copied()?;
        self.patterns.get(index)
    }

    pub fn route_for_pattern(&self, pattern: &TransitPattern) -> Option<&TransitRoute> {
        let index = self
            .indexes()
            .lookup
            .route_by_id
            .get(&pattern.route_id)
            .copied()?;
        self.routes.get(index)
    }

    pub fn route_of(&self, pattern: &TransitPattern) -> &TransitRoute {
        self.route_for_pattern(pattern)
            .expect("validated pattern route")
    }

    pub fn calendar(&self, id: &ServiceCalendarId) -> Option<&ServiceCalendar> {
        let index = self.indexes().lookup.calendar_by_id.get(id).copied()?;
        self.calendars.get(index)
    }

    pub fn modes_at_stop(&self, stop_id: &BoardingPointId) -> &[TransitMode] {
        self.indexes()
            .lookup
            .modes_by_stop
            .get(stop_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn scheduled_trip(&self, id: &TransitTripId) -> Option<&TransitTrip> {
        let index = self.indexes().lookup.trip_by_id.get(id).copied()?;
        self.trips.get(index)
    }

    pub(crate) fn scheduled_trip_at(&self, index: usize) -> Option<&TransitTrip> {
        self.trips.get(index)
    }

    pub(crate) fn scheduled_trip_indices(
        &self,
        pattern_id: &TransitPatternId,
        board_index: usize,
    ) -> &[usize] {
        self.indexes()
            .schedule
            .trip_indices_by_pattern_stop
            .get(pattern_id)
            .and_then(|by_stop| by_stop.get(board_index))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn frequency_indices(&self, pattern_id: &TransitPatternId) -> &[usize] {
        self.indexes()
            .schedule
            .frequency_indices_by_pattern
            .get(pattern_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
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

    fn indexes(&self) -> &TransitIndexes {
        self.indexes.get_or_init(|| TransitIndexes::build(self))
    }
}

#[cfg(test)]
impl TransitPattern {
    pub(crate) fn fixture(
        id: TransitPatternId,
        mode: TransitMode,
        label: impl Into<String>,
        headsign: impl Into<String>,
        stops: Vec<BoardingPointId>,
    ) -> Self {
        let label = label.into();
        let route_id = TransitRouteId::new(format!("fixture:{mode:?}:{label}"));
        let static_profile = SegmentProfile {
            segments: vec![
                SegmentTravelTime {
                    seconds: 120,
                    source: SegmentTimeSource::Estimated
                };
                stops.len().saturating_sub(1)
            ],
        };
        Self::new(id, route_id, None, headsign, stops, static_profile)
    }
}

#[cfg(test)]
impl TransitTrip {
    pub(crate) fn fixture_weekdays(
        id: TransitTripId,
        pattern_id: TransitPatternId,
        weekdays: Vec<Weekday>,
        stop_times: Vec<ScheduledStopTime>,
    ) -> Self {
        let encoded = weekdays
            .iter()
            .map(|day| day.number_from_monday().to_string())
            .collect::<Vec<_>>()
            .join(",");
        Self::new(
            id,
            pattern_id,
            ServiceCalendarId::new(format!("fixture-weekdays:{encoded}")),
            stop_times,
        )
    }
}

#[cfg(test)]
impl TransitNetwork {
    pub(crate) fn fixture(
        boundary: Time,
        stops: Vec<TransitStop>,
        mut patterns: Vec<TransitPattern>,
        trips: Vec<TransitTrip>,
    ) -> Self {
        let mut routes = HashMap::new();
        let coordinates = stops
            .iter()
            .map(|stop| (stop.id.clone(), stop.coordinate))
            .collect::<HashMap<_, _>>();
        for pattern in &mut patterns {
            let raw = pattern
                .route_id
                .as_str()
                .strip_prefix("fixture:")
                .expect("fixture route ID");
            let (mode, label) = raw.split_once(':').expect("fixture route fields");
            let mode = match mode {
                "Bus" => TransitMode::Bus,
                "Metro" => TransitMode::Metro,
                "Rail" => TransitMode::Rail,
                "HighSpeedRail" => TransitMode::HighSpeedRail,
                _ => panic!("unknown fixture mode"),
            };
            routes
                .entry(pattern.route_id.clone())
                .or_insert_with(|| TransitRoute {
                    id: pattern.route_id.clone(),
                    mode,
                    label: label.to_owned(),
                });
            let mut minimums = vec![u32::MAX; pattern.stops.len().saturating_sub(1)];
            for trip in trips.iter().filter(|trip| trip.pattern_id == pattern.id) {
                for (minimum, pair) in minimums.iter_mut().zip(trip.stop_times.windows(2)) {
                    let seconds = pair[1]
                        .arrival_seconds
                        .saturating_sub(pair[0].departure_seconds);
                    if seconds > 0 {
                        *minimum = (*minimum).min(seconds);
                    }
                }
            }
            let kmh = match mode {
                TransitMode::Bus => 60.0,
                TransitMode::Metro => 100.0,
                TransitMode::Rail => 160.0,
                TransitMode::HighSpeedRail => 350.0,
            };
            pattern.static_profile.segments = pattern
                .stops
                .windows(2)
                .enumerate()
                .map(|(index, pair)| {
                    if minimums[index] != u32::MAX {
                        SegmentTravelTime {
                            seconds: minimums[index],
                            source: SegmentTimeSource::Schedule,
                        }
                    } else {
                        let distance = coordinates
                            .get(&pair[0])
                            .zip(coordinates.get(&pair[1]))
                            .map_or(0.0, |(from, to)| from.haversine_distance_metres(*to));
                        SegmentTravelTime {
                            seconds: (distance / (kmh / 3.6)).ceil().max(1.0) as u32,
                            source: SegmentTimeSource::Estimated,
                        }
                    }
                })
                .collect();
        }
        let calendars = trips
            .iter()
            .map(|trip| trip.calendar_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|id| {
                let weekdays = id
                    .as_str()
                    .strip_prefix("fixture-weekdays:")
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|value| value.parse::<u8>().ok())
                    .filter_map(|number| match number {
                        1 => Some(Weekday::Monday),
                        2 => Some(Weekday::Tuesday),
                        3 => Some(Weekday::Wednesday),
                        4 => Some(Weekday::Thursday),
                        5 => Some(Weekday::Friday),
                        6 => Some(Weekday::Saturday),
                        7 => Some(Weekday::Sunday),
                        _ => None,
                    })
                    .collect();
                ServiceCalendar {
                    id,
                    weekdays,
                    start_date: None,
                    end_date: None,
                    added_dates: Vec::new(),
                    removed_dates: Vec::new(),
                }
            })
            .collect();
        Self::new(boundary, stops, patterns, trips)
            .with_routes(routes.into_values().collect())
            .with_calendars(calendars)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TransitIndexes {
    spatial: SpatialStopIndex,
    lookup: TransitLookupIndex,
    schedule: ScheduleIndex,
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
    modes_by_stop: HashMap<BoardingPointId, Vec<TransitMode>>,
    trip_by_id: HashMap<TransitTripId, usize>,
    calendar_by_id: HashMap<ServiceCalendarId, usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScheduleIndex {
    trip_indices_by_pattern_stop: HashMap<TransitPatternId, Vec<Vec<usize>>>,
    frequency_indices_by_pattern: HashMap<TransitPatternId, Vec<usize>>,
}

impl TransitIndexes {
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
        for (index, service) in network.patterns.iter().enumerate() {
            indexes
                .lookup
                .pattern_by_id
                .insert(service.id.clone(), index);
            let mode = network.routes[*indexes
                .lookup
                .route_by_id
                .get(&service.route_id)
                .expect("validated pattern route")]
            .mode;
            for stop_id in &service.stops {
                indexes
                    .lookup
                    .modes_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(mode);
            }
        }
        for modes in indexes.lookup.modes_by_stop.values_mut() {
            modes.sort_by_key(|mode| *mode as u8);
            modes.dedup();
        }
        for (trip_index, trip) in network.trips.iter().enumerate() {
            indexes
                .lookup
                .trip_by_id
                .insert(trip.id.clone(), trip_index);
            let Some(service_index) = indexes.lookup.pattern_by_id.get(&trip.pattern_id).copied()
            else {
                continue;
            };
            let service = &network.patterns[service_index];
            if trip.stop_times.len() != service.stops.len() {
                continue;
            }
            let trip_indices = indexes
                .schedule
                .trip_indices_by_pattern_stop
                .entry(trip.pattern_id.clone())
                .or_insert_with(|| vec![Vec::new(); service.stops.len()]);
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
    #[error("duplicate Transit Route {0:?}")]
    DuplicateRoute(TransitRouteId),
    #[error("Transit Pattern {0:?} references an unknown Route")]
    UnknownPatternRoute(TransitPatternId),
    #[error("Transit Pattern {0:?} has an invalid static profile")]
    InvalidPatternProfile(TransitPatternId),
    #[error("Transit Pattern {0:?} duplicates another route/direction/stop sequence")]
    DuplicatePatternTopology(TransitPatternId),
    #[error("duplicate Service Calendar {0:?}")]
    DuplicateCalendar(ServiceCalendarId),
    #[error("Service Calendar {0:?} has invalid dates")]
    InvalidCalendar(ServiceCalendarId),
    #[error("duplicate Transit Service {0:?}")]
    DuplicateService(TransitPatternId),
    #[error("duplicate Scheduled Trip {0:?}")]
    DuplicateTrip(TransitTripId),
    #[error("Transit Service {0:?} has fewer than two Boarding Points")]
    PatternTooShort(TransitPatternId),
    #[error("Transit Service {0:?} references an unknown Boarding Point")]
    UnknownPatternStop(TransitPatternId),
    #[error("Scheduled Trip references unknown Transit Service {0:?}")]
    UnknownTripService(TransitPatternId),
    #[error("Scheduled Trip {0:?} has an invalid shape or calendar")]
    InvalidTripShape(TransitTripId),
    #[error("Scheduled Trip {0:?} is not chronological")]
    NonChronologicalTrip(TransitTripId),
    #[error("Transit frequency references unknown Transit Service {0:?}")]
    UnknownFrequencyService(TransitPatternId),
    #[error("Transit frequency template {0:?} has an invalid shape or calendar")]
    InvalidFrequencyShape(TransitTripId),
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
