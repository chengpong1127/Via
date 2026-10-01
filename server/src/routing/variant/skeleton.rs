use time::{Duration, OffsetDateTime};

use crate::{
    domain::{ids::BoardingPointId, transit_timing::EffectiveTrip},
    routing::{JourneySegment, RealizedJourney},
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
    pub fn from_candidate(candidate: RealizedJourney) -> Result<Self, JourneySkeletonError> {
        if candidate.segments.is_empty() {
            return Err(JourneySkeletonError::NoSegments);
        }

        let mut anchors = Vec::with_capacity(candidate.segments.len() * 2 + 3);
        let mut movements = Vec::with_capacity(candidate.segments.len() + 2);
        let origin = push_anchor(
            &mut anchors,
            JourneyAnchorKind::Origin,
            candidate.departure_time,
        )?;
        let access = push_anchor(
            &mut anchors,
            JourneyAnchorKind::BoardingPoint {
                id: candidate.access.stop_id.clone(),
            },
            candidate.access.available_at,
        )?;
        movements.push(JourneyMovement {
            from_anchor: origin,
            to_anchor: access,
            departure_at: candidate.departure_time,
            arrival_at: candidate.access.available_at,
            kind: JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Access,
                duration_seconds: candidate.access.walking_seconds,
                distance_metres: candidate.access.distance_metres,
            },
            street_path: None,
        });

        let mut current_anchor = access;
        let mut current_stop = candidate.access.stop_id.clone();
        let mut current_time = candidate.access.available_at;

        for (segment_index, segment) in candidate.segments.iter().enumerate() {
            match segment {
                JourneySegment::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => {
                    let Some(board) = trip.stop_times.get(*board_index) else {
                        return Err(JourneySkeletonError::InvalidTransitSegment { segment_index });
                    };
                    let Some(alight) = trip.stop_times.get(*alight_index) else {
                        return Err(JourneySkeletonError::InvalidTransitSegment { segment_index });
                    };
                    if board_index >= alight_index {
                        return Err(JourneySkeletonError::InvalidTransitSegment { segment_index });
                    }
                    if board.stop_id != current_stop {
                        return Err(JourneySkeletonError::UnexpectedBoardingPoint {
                            segment_index,
                        });
                    }
                    if board.departure_at < current_time || alight.arrival_at < board.departure_at {
                        return Err(JourneySkeletonError::NonMonotonicTimes);
                    }

                    let alighting = push_anchor(
                        &mut anchors,
                        JourneyAnchorKind::BoardingPoint {
                            id: alight.stop_id.clone(),
                        },
                        alight.arrival_at,
                    )?;
                    movements.push(JourneyMovement {
                        from_anchor: current_anchor,
                        to_anchor: alighting,
                        departure_at: board.departure_at,
                        arrival_at: alight.arrival_at,
                        kind: JourneyMovementKind::Transit {
                            trip: Arc::new(trip.clone()),
                            board_index: *board_index,
                            alight_index: *alight_index,
                        },
                        street_path: None,
                    });
                    current_anchor = alighting;
                    current_stop = alight.stop_id.clone();
                    current_time = alight.arrival_at;
                }
                JourneySegment::WalkingTransfer { shortcut } => {
                    if shortcut.from != current_stop {
                        return Err(JourneySkeletonError::UnexpectedTransferSource {
                            segment_index,
                        });
                    }
                    let arrival_at =
                        current_time + Duration::seconds(i64::from(shortcut.duration_seconds));
                    let target = push_anchor(
                        &mut anchors,
                        JourneyAnchorKind::BoardingPoint {
                            id: shortcut.to.clone(),
                        },
                        arrival_at,
                    )?;
                    movements.push(JourneyMovement {
                        from_anchor: current_anchor,
                        to_anchor: target,
                        departure_at: current_time,
                        arrival_at,
                        kind: JourneyMovementKind::Walking {
                            kind: WalkingMovementKind::Transfer,
                            duration_seconds: shortcut.duration_seconds,
                            distance_metres: shortcut.distance_metres,
                        },
                        street_path: None,
                    });
                    current_anchor = target;
                    current_stop = shortcut.to.clone();
                    current_time = arrival_at;
                }
            }
        }

        if current_stop != candidate.egress.stop_id {
            return Err(JourneySkeletonError::UnexpectedEgressSource);
        }
        let expected_arrival =
            current_time + Duration::seconds(i64::from(candidate.egress.walking_seconds));
        if expected_arrival != candidate.arrival_time {
            return Err(JourneySkeletonError::EgressArrivalMismatch);
        }
        let destination = push_anchor(
            &mut anchors,
            JourneyAnchorKind::Destination,
            candidate.arrival_time,
        )?;
        movements.push(JourneyMovement {
            from_anchor: current_anchor,
            to_anchor: destination,
            departure_at: current_time,
            arrival_at: candidate.arrival_time,
            kind: JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Egress,
                duration_seconds: candidate.egress.walking_seconds,
                distance_metres: candidate.egress.distance_metres,
            },
            street_path: None,
        });

        Ok(Self {
            anchors,
            movements,
            departure_at: candidate.departure_time,
        })
    }

    pub fn from_direct_walking(
        path: StreetPath,
        departure_at: OffsetDateTime,
    ) -> Result<Self, JourneySkeletonError> {
        let arrival_at = departure_at + Duration::seconds(i64::from(path.duration.0));
        let mut anchors = Vec::with_capacity(2);
        let origin = push_anchor(&mut anchors, JourneyAnchorKind::Origin, departure_at)?;
        let destination = push_anchor(&mut anchors, JourneyAnchorKind::Destination, arrival_at)?;
        Ok(Self {
            anchors,
            movements: vec![JourneyMovement {
                from_anchor: origin,
                to_anchor: destination,
                departure_at,
                arrival_at,
                kind: JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Direct,
                    duration_seconds: path.duration.0,
                    distance_metres: path.distance.0,
                },
                street_path: Some(Arc::new(path)),
            }],
            departure_at,
        })
    }

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

fn push_anchor(
    anchors: &mut Vec<JourneyAnchor>,
    kind: JourneyAnchorKind,
    reached_at: OffsetDateTime,
) -> Result<usize, JourneySkeletonError> {
    if anchors
        .last()
        .is_some_and(|previous| reached_at < previous.reached_at)
    {
        return Err(JourneySkeletonError::NonMonotonicTimes);
    }
    let index = anchors.len();
    anchors.push(JourneyAnchor { kind, reached_at });
    Ok(index)
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum JourneySkeletonError {
    #[error("journey candidate has no segments")]
    NoSegments,
    #[error("journey candidate has an invalid transit segment at index {segment_index}")]
    InvalidTransitSegment { segment_index: usize },
    #[error("journey segment {segment_index} boards at an unexpected boarding point")]
    UnexpectedBoardingPoint { segment_index: usize },
    #[error("journey segment {segment_index} transfers from an unexpected boarding point")]
    UnexpectedTransferSource { segment_index: usize },
    #[error("journey candidate egress starts at an unexpected boarding point")]
    UnexpectedEgressSource,
    #[error("journey candidate contains non-monotonic times")]
    NonMonotonicTimes,
    #[error("journey candidate egress does not reach its declared arrival time")]
    EgressArrivalMismatch,
}
