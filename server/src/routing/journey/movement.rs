use time::OffsetDateTime;

use crate::domain::{ids::BoardingPointId, transit::timing::EffectiveTrip};

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub enum JourneyAnchorKind {
    Origin,
    BoardingPoint { id: BoardingPointId },
    Destination,
}

impl JourneyAnchorKind {
    pub(in crate::routing) fn boarding_point(&self) -> Option<&BoardingPointId> {
        match self {
            Self::BoardingPoint { id } => Some(id),
            Self::Origin | Self::Destination => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JourneyAnchor {
    pub kind: JourneyAnchorKind,
    pub reached_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WalkingMovementKind {
    Access,
    Transfer,
    Egress,
    Direct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JourneyMovementKind {
    BikeShare {
        movement: BikeShareMovement,
    },
    Walking {
        kind: WalkingMovementKind,
        duration_seconds: u32,
        distance_metres: u32,
    },
    Transit {
        trip: Arc<EffectiveTrip>,
        board_index: usize,
        alight_index: usize,
    },
}

impl JourneyMovementKind {
    pub(in crate::routing) fn walking_seconds(&self) -> u32 {
        match self {
            Self::Walking {
                duration_seconds, ..
            } => *duration_seconds,
            Self::BikeShare { movement } => movement.walking_seconds(),
            Self::Transit { .. } => 0,
        }
    }

    pub(in crate::routing) fn bicycle_seconds(&self) -> u32 {
        match self {
            Self::BikeShare { movement } => movement.bicycle_duration_seconds,
            Self::Walking { .. } | Self::Transit { .. } => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JourneyMovement {
    pub from_anchor: usize,
    pub to_anchor: usize,
    pub departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub kind: JourneyMovementKind,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RealizedJourneyError {
    #[error("journey has an invalid realized path")]
    InvalidRealizedPath,
    #[error("journey has no realized movements")]
    NoMovements,
}
