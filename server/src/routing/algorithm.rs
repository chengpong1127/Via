//! The small algorithm boundary shared by all future routing algorithms.
//!
//! Nothing in this module describes a Trip-Based queue, RAPTOR round, CSA
//! connection, or generic graph node. Algorithms receive access/egress facts
//! and return feasible candidate journeys made from domain transit trips and
//! transfer shortcuts.

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::api::RequestCoordinate;
use crate::domain::{
    ids::{BoardingPointId, TransitServiceId},
    realtime::NormalizedRealtimeValue,
    transit_timing::TimingBasis,
};

use super::{
    EffectiveTrip, MobilityPath, TransferShortcut,
    bike::{BikeShareTransfer, MobilityLeg},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    pub stop_id: BoardingPointId,
    pub departure_at: OffsetDateTime,
    pub available_at: OffsetDateTime,
    pub walking_seconds: u32,
    pub bike_seconds: u32,
    pub distance_metres: u32,
    pub path: Option<MobilityPath>,
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
            bike_seconds: 0,
            distance_metres,
            path: None,
        }
    }

    pub fn with_path(
        stop_id: BoardingPointId,
        departure_at: OffsetDateTime,
        path: MobilityPath,
        distance_metres: u32,
    ) -> Self {
        Self {
            stop_id,
            departure_at,
            available_at: departure_at
                + time::Duration::seconds(i64::from(path.duration_seconds())),
            walking_seconds: path.walking_seconds(),
            bike_seconds: path.bike_seconds(),
            distance_metres,
            path: Some(path),
        }
    }

    pub fn required_station_ids(&self) -> Vec<crate::domain::ids::BikeShareStationId> {
        self.path
            .as_ref()
            .map(MobilityPath::required_station_ids)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressPoint {
    pub stop_id: BoardingPointId,
    pub walking_seconds: u32,
    pub bike_seconds: u32,
    pub distance_metres: u32,
    pub path: Option<MobilityPath>,
}

impl EgressPoint {
    pub fn new(stop_id: BoardingPointId, walking_seconds: u32, distance_metres: u32) -> Self {
        Self {
            stop_id,
            walking_seconds,
            bike_seconds: 0,
            distance_metres,
            path: None,
        }
    }

    pub fn with_path(stop_id: BoardingPointId, path: MobilityPath, distance_metres: u32) -> Self {
        Self {
            stop_id,
            walking_seconds: path.walking_seconds(),
            bike_seconds: path.bike_seconds(),
            distance_metres,
            path: Some(path),
        }
    }

    pub fn required_station_ids(&self) -> Vec<crate::domain::ids::BikeShareStationId> {
        self.path
            .as_ref()
            .map(MobilityPath::required_station_ids)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlgorithmQuery {
    pub origin_access: Vec<AccessPoint>,
    pub destination_egress: Vec<EgressPoint>,
    pub departure_time: OffsetDateTime,
    pub latest_arrival: Option<OffsetDateTime>,
    pub max_transfers: usize,
    pub max_candidates: usize,
    pub max_expansions: usize,
    pub max_journey_duration_seconds: u32,
    pub destination: Option<RequestCoordinate>,
}

impl AlgorithmQuery {
    pub fn new(
        origin_access: Vec<AccessPoint>,
        destination_egress: Vec<EgressPoint>,
        departure_time: OffsetDateTime,
    ) -> Self {
        Self {
            origin_access,
            destination_egress,
            departure_time,
            latest_arrival: None,
            max_transfers: 4,
            max_candidates: 16,
            max_expansions: 2_000,
            max_journey_duration_seconds: 4 * 60 * 60,
            destination: None,
        }
    }

    pub fn with_limits(
        mut self,
        max_transfers: usize,
        max_candidates: usize,
        max_expansions: usize,
        max_journey_duration_seconds: u32,
    ) -> Self {
        self.max_transfers = max_transfers;
        self.max_candidates = max_candidates;
        self.max_expansions = max_expansions;
        self.max_journey_duration_seconds = max_journey_duration_seconds;
        self
    }

    pub fn with_latest_arrival(mut self, latest_arrival: OffsetDateTime) -> Self {
        self.latest_arrival = Some(latest_arrival);
        self
    }

    pub fn with_destination(mut self, destination: RequestCoordinate) -> Self {
        self.destination = Some(destination);
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
    BikeShareTransfer {
        transfer: BikeShareTransfer,
    },
}

impl JourneySegment {
    pub fn service_id(&self) -> Option<&TransitServiceId> {
        match self {
            Self::Transit { trip, .. } => Some(&trip.service_id),
            Self::WalkingTransfer { .. } | Self::BikeShareTransfer { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JourneyCandidate {
    pub access: AccessPoint,
    pub egress: EgressPoint,
    pub segments: Vec<JourneySegment>,
    pub departure_time: OffsetDateTime,
    pub arrival_time: OffsetDateTime,
    pub walking_seconds: u32,
    pub bike_seconds: u32,
    pub transfer_count: u32,
    pub uncertainty_seconds: Option<u32>,
    pub evidence: Vec<NormalizedRealtimeValue>,
    pub timing_bases: Vec<TimingBasis>,
}

impl JourneyCandidate {
    pub fn transit_service_sequence(&self) -> Vec<TransitServiceId> {
        self.segments
            .iter()
            .filter_map(JourneySegment::service_id)
            .cloned()
            .collect()
    }

    pub fn dominates(&self, other: &Self) -> bool {
        let no_worse = self.arrival_time <= other.arrival_time
            && self.walking_seconds <= other.walking_seconds
            && self.bike_seconds <= other.bike_seconds
            && self.transfer_count <= other.transfer_count;
        let strictly_better = self.arrival_time < other.arrival_time
            || self.walking_seconds < other.walking_seconds
            || self.bike_seconds < other.bike_seconds
            || self.transfer_count < other.transfer_count;
        no_worse && strictly_better
    }

    pub fn signature(&self) -> String {
        let access = format!(
            "access:{}:{}:{}:{}",
            self.access.stop_id.as_str(),
            self.access.walking_seconds,
            self.access.bike_seconds,
            endpoint_path_signature(self.access.path.as_ref()),
        );
        let egress = format!(
            "egress:{}:{}:{}:{}",
            self.egress.stop_id.as_str(),
            self.egress.walking_seconds,
            self.egress.bike_seconds,
            endpoint_path_signature(self.egress.path.as_ref()),
        );
        let mut parts = vec![access];
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
            JourneySegment::BikeShareTransfer { transfer } => format!(
                "bike-transfer:{}:{}:{}:{}:{}:{}",
                transfer.from_stop_id.as_str(),
                transfer.to_stop_id.as_str(),
                transfer.pickup_station_id.as_str(),
                transfer.return_station_id.as_str(),
                transfer.bike_seconds,
                transfer.duration_seconds(),
            ),
        }));
        parts.push(egress);
        parts.join("|")
    }
}

fn endpoint_path_signature(path: Option<&MobilityPath>) -> String {
    let Some(path) = path else {
        return "walking".into();
    };
    path.legs
        .iter()
        .map(|leg| match leg {
            MobilityLeg::Walk {
                duration_seconds,
                distance_metres,
            } => format!("walk:{duration_seconds}:{distance_metres}"),
            MobilityLeg::BikeShare {
                pickup_station_id,
                return_station_id,
                duration_seconds,
                distance_metres,
            } => format!(
                "bike:{}:{}:{duration_seconds}:{distance_metres}",
                pickup_station_id.as_str(),
                return_station_id.as_str()
            ),
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoutingError {
    #[error("routing query has no usable access or egress")]
    InvalidQuery,
    #[error("routing departure resolution failed: {0}")]
    Departure(#[from] super::DepartureError),
    #[error("routing expansion limit reached")]
    ExpansionLimit,
}

#[async_trait]
pub trait RoutingAlgorithm: Send + Sync {
    async fn route(&self, query: AlgorithmQuery) -> Result<Vec<JourneyCandidate>, RoutingError>;
}
