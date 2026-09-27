//! Immutable, provider-neutral transit data consumed by the routing layer.
//!
//! Static adapters build validated `TransitNetwork` artifacts. Requests read one
//! immutable revision and resolve departures through the timing boundary.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use time::{Date, Time, Weekday};

use super::ids::{
    BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
};
use super::location::RequestCoordinate;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct NetworkRevision {
    pub static_network_revision: String,
    pub schedule_revision: String,
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
        }
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
        let mut pattern_ids = HashSet::new();
        let mut pattern_topologies = HashSet::new();
        for pattern in &self.patterns {
            if !pattern_ids.insert(pattern.id.clone()) {
                return Err(TransitNetworkValidationError::DuplicatePattern(
                    pattern.id.clone(),
                ));
            }
            if pattern.stops.len() < 2 {
                return Err(TransitNetworkValidationError::PatternTooShort(
                    pattern.id.clone(),
                ));
            }
            if !route_ids.contains(&pattern.route_id) {
                return Err(TransitNetworkValidationError::UnknownPatternRoute(
                    pattern.id.clone(),
                ));
            }
            if !pattern_topologies.insert((
                pattern.route_id.clone(),
                pattern.direction,
                pattern.stops.clone(),
            )) {
                return Err(TransitNetworkValidationError::DuplicatePatternTopology(
                    pattern.id.clone(),
                ));
            }
            if pattern.static_profile.segments.len() != pattern.stops.len() - 1
                || pattern
                    .static_profile
                    .segments
                    .iter()
                    .any(|segment| segment.seconds == 0)
            {
                return Err(TransitNetworkValidationError::InvalidPatternProfile(
                    pattern.id.clone(),
                ));
            }
            if pattern
                .stops
                .iter()
                .any(|stop_id| !stop_ids.contains(stop_id))
            {
                return Err(TransitNetworkValidationError::UnknownPatternStop(
                    pattern.id.clone(),
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
            let Some(pattern) = pattern_by_id.get(&trip.pattern_id).copied() else {
                return Err(TransitNetworkValidationError::UnknownTripPattern(
                    trip.pattern_id.clone(),
                ));
            };
            if !calendar_ids.contains(&trip.calendar_id)
                || trip.stop_times.len() != pattern.stops.len()
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
            let Some(pattern) = pattern_by_id.get(&frequency.pattern_id).copied() else {
                return Err(TransitNetworkValidationError::UnknownFrequencyPattern(
                    frequency.pattern_id.clone(),
                ));
            };
            if !frequency_ids.insert(frequency.id.clone())
                || frequency.start_seconds >= frequency.end_seconds
                || frequency.headway_seconds == 0
                || frequency.template_stop_times.len() != pattern.stops.len()
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
    /// an artifact. Ordered Patterns and stop times are preserved.
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

    pub fn revisions(&self) -> (&str, &str) {
        (&self.static_network_revision, &self.schedule_revision)
    }

    pub fn revision(&self) -> NetworkRevision {
        NetworkRevision {
            static_network_revision: self.static_network_revision.clone(),
            schedule_revision: self.schedule_revision.clone(),
        }
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
    #[error("duplicate Transit Pattern {0:?}")]
    DuplicatePattern(TransitPatternId),
    #[error("duplicate Scheduled Trip {0:?}")]
    DuplicateTrip(TransitTripId),
    #[error("Transit Pattern {0:?} has fewer than two Boarding Points")]
    PatternTooShort(TransitPatternId),
    #[error("Transit Pattern {0:?} references an unknown Boarding Point")]
    UnknownPatternStop(TransitPatternId),
    #[error("Scheduled Trip references unknown Transit Pattern {0:?}")]
    UnknownTripPattern(TransitPatternId),
    #[error("Scheduled Trip {0:?} has an invalid shape or calendar")]
    InvalidTripShape(TransitTripId),
    #[error("Scheduled Trip {0:?} is not chronological")]
    NonChronologicalTrip(TransitTripId),
    #[error("Transit frequency references unknown Transit Pattern {0:?}")]
    UnknownFrequencyPattern(TransitPatternId),
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
