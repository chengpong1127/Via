use std::sync::Arc;

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::{
    api::{PlanningRequest, RequestCoordinate},
    domain::ids::BoardingPointId,
};

use super::super::{JourneyCandidate, JourneySegment};

/// The only locations an augmenter needs to reason about.
#[derive(Clone, Debug, PartialEq)]
pub enum JourneyAnchorKind {
    Origin { coordinate: RequestCoordinate },
    BoardingPoint { id: BoardingPointId },
    Destination { coordinate: RequestCoordinate },
}

#[derive(Clone, Debug, PartialEq)]
pub struct JourneyAnchor {
    pub kind: JourneyAnchorKind,
    pub reached_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkingMovementKind {
    Access,
    Transfer,
    Egress,
}

impl WalkingMovementKind {
    pub fn is_access(self) -> bool {
        matches!(self, Self::Access)
    }

    pub fn is_transfer(self) -> bool {
        matches!(self, Self::Transfer)
    }

    pub fn is_egress(self) -> bool {
        matches!(self, Self::Egress)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JourneyMovementKind {
    Walking {
        kind: WalkingMovementKind,
        duration_seconds: u32,
        distance_metres: u32,
    },
    Transit {
        /// Index into the baseline candidate's segment sequence.
        segment_index: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JourneyMovement {
    pub from_anchor: usize,
    pub to_anchor: usize,
    pub departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub kind: JourneyMovementKind,
}

/// A compact projection of a candidate into augmentation-relevant anchors.
///
/// The candidate remains the single owner of the full transit objects. Baseline
/// movements point into its segment sequence instead of copying `EffectiveTrip`
/// values into the skeleton.
#[derive(Clone, Debug, PartialEq)]
pub struct JourneySkeleton {
    anchors: Vec<JourneyAnchor>,
    movements: Vec<JourneyMovement>,
    candidate: Arc<JourneyCandidate>,
}

impl JourneySkeleton {
    pub fn from_candidate(
        request: &PlanningRequest,
        candidate: JourneyCandidate,
    ) -> Result<Self, JourneySkeletonError> {
        if candidate.segments.is_empty() {
            return Err(JourneySkeletonError::NoSegments);
        }

        let mut anchors = Vec::with_capacity(candidate.segments.len() * 2 + 3);
        let mut movements = Vec::with_capacity(candidate.segments.len() + 2);
        let origin = push_anchor(
            &mut anchors,
            JourneyAnchorKind::Origin {
                coordinate: request.origin.coordinate,
            },
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
                        kind: JourneyMovementKind::Transit { segment_index },
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
                            distance_metres: 0,
                        },
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
            JourneyAnchorKind::Destination {
                coordinate: request.destination.coordinate,
            },
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
        });

        Ok(Self {
            anchors,
            movements,
            candidate: Arc::new(candidate),
        })
    }

    pub fn anchors(&self) -> &[JourneyAnchor] {
        &self.anchors
    }

    pub fn movements(&self) -> &[JourneyMovement] {
        &self.movements
    }

    pub(crate) fn candidate(&self) -> &JourneyCandidate {
        &self.candidate
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

/// One selectable path through a skeleton.
///
/// Path steps carry explicit anchor endpoints. A future augmentation can use a
/// single step spanning any contiguous interval `A_i → A_j`; the baseline path
/// currently contains one step for each projected movement.
#[derive(Clone, Debug, PartialEq)]
pub struct JourneyVariant {
    skeleton: Arc<JourneySkeleton>,
    path: Vec<JourneyPathStep>,
}

impl JourneyVariant {
    pub fn baseline(skeleton: JourneySkeleton) -> Self {
        let path = skeleton
            .movements()
            .iter()
            .enumerate()
            .map(|(movement_index, movement)| JourneyPathStep {
                from_anchor: movement.from_anchor,
                to_anchor: movement.to_anchor,
                movement: JourneyPathMovement::Baseline { movement_index },
            })
            .collect();
        Self {
            skeleton: Arc::new(skeleton),
            path,
        }
    }

    pub fn skeleton(&self) -> &JourneySkeleton {
        &self.skeleton
    }

    pub fn path(&self) -> &[JourneyPathStep] {
        &self.path
    }

    pub fn is_baseline(&self) -> bool {
        self.path.len() == self.skeleton.movements().len()
            && self.path.iter().enumerate().all(|(index, step)| {
                let movement = &self.skeleton.movements()[index];
                step.from_anchor == movement.from_anchor
                    && step.to_anchor == movement.to_anchor
                    && step.movement
                        == JourneyPathMovement::Baseline {
                            movement_index: index,
                        }
            })
    }

    pub fn baseline_signature(&self) -> String {
        self.skeleton.candidate().signature()
    }

    pub(crate) fn candidate(&self) -> Option<&JourneyCandidate> {
        self.is_baseline().then(|| self.skeleton.candidate())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JourneyPathStep {
    pub from_anchor: usize,
    pub to_anchor: usize,
    pub movement: JourneyPathMovement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JourneyPathMovement {
    Baseline { movement_index: usize },
}
