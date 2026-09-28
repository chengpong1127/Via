use time::OffsetDateTime;

use crate::domain::{
    ids::BoardingPointId,
    realtime::NormalizedRealtimeValue,
    transit::TransitMode,
    transit_timing::{DepartureError, EffectiveTrip, TimingBasis, TimingReliability},
};

use super::transfers::TransferShortcut;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    pub stop_id: BoardingPointId,
    pub departure_at: OffsetDateTime,
    pub available_at: OffsetDateTime,
    pub walking_seconds: u32,
    pub distance_metres: u32,
}

impl AccessPoint {
    pub fn new(
        stop_id: BoardingPointId,
        departure_at: OffsetDateTime,
        available_at: OffsetDateTime,
        walking_seconds: u32,
        distance_metres: u32,
    ) -> Self {
        Self {
            stop_id,
            departure_at,
            available_at,
            walking_seconds,
            distance_metres,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressPoint {
    pub stop_id: BoardingPointId,
    pub walking_seconds: u32,
    pub distance_metres: u32,
}

impl EgressPoint {
    pub fn new(stop_id: BoardingPointId, walking_seconds: u32, distance_metres: u32) -> Self {
        Self {
            stop_id,
            walking_seconds,
            distance_metres,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransitQuery {
    pub origin_access: Vec<AccessPoint>,
    pub destination_egress: Vec<EgressPoint>,
    pub allowed_transit_modes: Vec<TransitMode>,

    pub allow_bike_share: bool,
    pub departure_time: OffsetDateTime,
    pub latest_arrival: Option<OffsetDateTime>,
    pub max_transfers: usize,

    pub max_static_corridors: usize,
    pub max_static_search_expansions: usize,
    pub max_candidates: usize,
    pub max_expansions: usize,
    pub max_departures_per_query: usize,
    pub max_journey_duration_seconds: u32,
}

impl TransitQuery {
    pub fn new(
        origin_access: Vec<AccessPoint>,
        destination_egress: Vec<EgressPoint>,
        departure_time: OffsetDateTime,
    ) -> Self {
        Self {
            origin_access,
            destination_egress,
            allowed_transit_modes: vec![
                TransitMode::Bus,
                TransitMode::Metro,
                TransitMode::Rail,
                TransitMode::HighSpeedRail,
            ],
            allow_bike_share: false,
            departure_time,
            latest_arrival: None,
            max_transfers: 4,
            max_static_corridors: 100,
            max_static_search_expansions: 20_000,
            max_candidates: 16,
            max_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
        }
    }

    pub fn with_allowed_transit_modes(
        mut self,
        modes: impl IntoIterator<Item = TransitMode>,
    ) -> Self {
        self.allowed_transit_modes = modes.into_iter().collect();
        self.allowed_transit_modes.sort();
        self.allowed_transit_modes.dedup();
        self
    }

    pub fn with_bike_share(mut self, allow: bool) -> Self {
        self.allow_bike_share = allow;
        self
    }

    pub fn with_limits(
        mut self,
        max_transfers: usize,
        max_candidates: usize,
        max_expansions: usize,
        max_departures_per_query: usize,
        max_journey_duration_seconds: u32,
    ) -> Self {
        self.max_transfers = max_transfers;
        self.max_candidates = max_candidates;
        self.max_expansions = max_expansions;
        self.max_departures_per_query = max_departures_per_query;
        self.max_journey_duration_seconds = max_journey_duration_seconds;
        self
    }

    pub fn with_max_static_corridors(mut self, max_static_corridors: usize) -> Self {
        self.max_static_corridors = max_static_corridors.max(1);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JourneySegment {
    Transit {
        trip: EffectiveTrip,
        board_index: usize,
        alight_index: usize,
    },
    WalkingTransfer {
        shortcut: TransferShortcut,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JourneyCandidate {
    pub access: AccessPoint,
    pub egress: EgressPoint,
    pub segments: Vec<JourneySegment>,
    pub departure_time: OffsetDateTime,
    pub arrival_time: OffsetDateTime,
    pub walking_seconds: u32,
    pub transfer_count: u32,
    pub uncertainty_seconds: Option<u32>,
    pub evidence: Vec<NormalizedRealtimeValue>,
    pub timing_bases: Vec<TimingBasis>,
    pub timing_reliability: TimingReliability,
}

impl JourneyCandidate {
    pub fn dominates(&self, other: &Self) -> bool {
        let no_worse = self.arrival_time <= other.arrival_time
            && self.walking_seconds <= other.walking_seconds
            && self.transfer_count <= other.transfer_count;
        let strictly_better = self.arrival_time < other.arrival_time
            || self.walking_seconds < other.walking_seconds
            || self.transfer_count < other.transfer_count;
        no_worse && strictly_better
    }

    pub fn signature(&self) -> String {
        let mut parts = vec![format!(
            "access:{}:{}",
            self.access.stop_id.as_str(),
            self.access.walking_seconds
        )];
        parts.extend(self.segments.iter().map(|segment| match segment {
            JourneySegment::Transit {
                trip,
                board_index,
                alight_index,
            } => format!(
                "transit:{}:{}:{}:{}",
                trip.id.as_str(),
                board_index,
                alight_index,
                trip.stop_times[*board_index]
                    .departure_at
                    .unix_timestamp_nanos()
            ),
            JourneySegment::WalkingTransfer { shortcut } => format!(
                "transfer:{}:{}:{}",
                shortcut.from.as_str(),
                shortcut.to.as_str(),
                shortcut.duration_seconds
            ),
        }));
        parts.push(format!(
            "egress:{}:{}",
            self.egress.stop_id.as_str(),
            self.egress.walking_seconds
        ));
        parts.join("|")
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoutingError {
    #[error("routing query has no usable access or egress")]
    InvalidQuery,
    #[error("static corridor search exceeded its safety expansion bound")]
    StaticSearchLimit,
    #[error("fixed-corridor evaluation exceeded its safety expansion bound")]
    Phase2EvaluationLimit,
    #[error("timing snapshot could not be loaded: {0}")]
    Timing(#[from] DepartureError),
}
