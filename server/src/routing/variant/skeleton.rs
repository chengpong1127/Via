use time::OffsetDateTime;

use crate::{
    domain::{ids::BoardingPointId, transit_timing::EffectiveTrip},
    street::StreetPath,
};

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub enum JourneyAnchorKind {
    Origin,
    BoardingPoint { id: BoardingPointId },
    Destination,
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
        shortcut: BikeShortcut,
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

#[derive(Clone, Debug, PartialEq)]
pub struct JourneyMovement {
    pub from_anchor: usize,
    pub to_anchor: usize,
    pub departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub kind: JourneyMovementKind,
    pub street_path: Option<Arc<StreetPath>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JourneySkeleton {
    pub(super) anchors: Vec<JourneyAnchor>,
    pub(super) movements: Vec<JourneyMovement>,
    pub(super) departure_at: OffsetDateTime,
}

impl JourneySkeleton {
    pub fn anchors(&self) -> &[JourneyAnchor] {
        &self.anchors
    }

    pub fn movements(&self) -> &[JourneyMovement] {
        &self.movements
    }

    pub fn departure_at(&self) -> OffsetDateTime {
        self.departure_at
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum JourneySkeletonError {
    #[error("journey has an invalid realized path")]
    InvalidRealizedPath,
    #[error("journey has no realized movements")]
    NoMovements,
}
