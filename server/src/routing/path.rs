use std::sync::Arc;

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::{
    domain::{
        bikeshare::BikeWalkingAccess,
        ids::{BikeStationId, BoardingPointId},
        transit_timing::EffectiveTrip,
    },
    street::StreetPath,
};

use super::{JourneyCandidate, JourneySegment};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    anchors: Vec<JourneyAnchor>,
    movements: Vec<JourneyMovement>,
    departure_at: OffsetDateTime,
}

impl JourneySkeleton {
    pub fn from_candidate(candidate: JourneyCandidate) -> Result<Self, JourneySkeletonError> {
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
                            distance_metres: 0,
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

    pub(crate) fn signature(&self) -> String {
        if self.is_baseline() {
            return self.skeleton.baseline_signature();
        }
        let path = self
            .path
            .iter()
            .map(|step| match &step.movement {
                JourneyPathMovement::Baseline { movement_index } => format!(
                    "baseline:{}",
                    self.skeleton.baseline_movement_signature(*movement_index)
                ),
                JourneyPathMovement::BikeShare { shortcut } => format!(
                    "bike:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                    anchor_signature(&self.skeleton.anchors()[step.from_anchor].kind),
                    anchor_signature(&self.skeleton.anchors()[step.to_anchor].kind),
                    shortcut.pickup_station_id.as_str(),
                    shortcut.dropoff_station_id.as_str(),
                    shortcut.walk_to_pickup.duration_seconds,
                    shortcut.walk_to_pickup.distance_metres,
                    shortcut.bicycle_duration_seconds,
                    shortcut.bicycle_distance_metres,
                    shortcut.walk_from_dropoff.duration_seconds,
                    shortcut.walk_from_dropoff.distance_metres,
                    shortcut.total_seconds,
                ),
            })
            .collect::<Vec<_>>()
            .join("|");
        format!("path:v2:{path}")
    }

    pub fn evaluate_timing(&self) -> Result<JourneyVariantTiming, JourneyTimingError> {
        let mut current_anchor = 0;
        let mut current_time = self.skeleton.anchors()[0].reached_at;
        let mut timing = JourneyVariantTiming {
            departure_at: current_time,
            arrival_at: current_time,
            walking_seconds: 0,
            bicycle_seconds: 0,
            bicycle_distance_metres: 0,
            bike_share_uses: 0,
        };

        for step in &self.path {
            if step.from_anchor != current_anchor || step.to_anchor <= step.from_anchor {
                return Err(JourneyTimingError::NonContiguousPath);
            }

            match &step.movement {
                JourneyPathMovement::Baseline { movement_index } => {
                    let movement = self.skeleton.movements().get(*movement_index).ok_or(
                        JourneyTimingError::InvalidBaselineMovement {
                            movement_index: *movement_index,
                        },
                    )?;
                    if movement.from_anchor != step.from_anchor
                        || movement.to_anchor != step.to_anchor
                    {
                        return Err(JourneyTimingError::InvalidBaselineMovement {
                            movement_index: *movement_index,
                        });
                    }

                    match movement.kind {
                        JourneyMovementKind::Walking {
                            duration_seconds, ..
                        } => {
                            current_time += Duration::seconds(i64::from(duration_seconds));
                            timing.walking_seconds =
                                timing.walking_seconds.saturating_add(duration_seconds);
                        }
                        JourneyMovementKind::Transit { .. } => {
                            if current_time > movement.departure_at {
                                return Err(JourneyTimingError::MissedTransit {
                                    movement_index: *movement_index,
                                });
                            }
                            current_time = movement.arrival_at;
                        }
                    }
                }
                JourneyPathMovement::BikeShare { shortcut } => {
                    if shortcut.from_anchor != step.from_anchor
                        || shortcut.to_anchor != step.to_anchor
                    {
                        return Err(JourneyTimingError::InvalidBikeShortcut);
                    }
                    current_time += Duration::seconds(i64::from(shortcut.total_seconds));
                    timing.walking_seconds = timing
                        .walking_seconds
                        .saturating_add(shortcut.walk_to_pickup.duration_seconds)
                        .saturating_add(shortcut.walk_from_dropoff.duration_seconds);
                    timing.bicycle_seconds = timing
                        .bicycle_seconds
                        .saturating_add(shortcut.bicycle_duration_seconds);
                    timing.bicycle_distance_metres = timing
                        .bicycle_distance_metres
                        .saturating_add(shortcut.bicycle_distance_metres);
                    timing.bike_share_uses = timing.bike_share_uses.saturating_add(1);
                }
            }

            current_anchor = step.to_anchor;
        }

        if current_anchor != self.skeleton.anchors().len().saturating_sub(1) {
            return Err(JourneyTimingError::IncompletePath);
        }
        timing.arrival_at = current_time;
        Ok(timing)
    }

    pub(crate) fn with_shortcut(baseline: &Self, shortcut: BikeShortcut) -> Option<Self> {
        if shortcut.to_anchor <= shortcut.from_anchor {
            return None;
        }
        let mut path = baseline.path.clone();
        let start = path
            .iter()
            .position(|step| step.from_anchor == shortcut.from_anchor)?;
        let mut end = start;
        let mut anchor = shortcut.from_anchor;
        while anchor < shortcut.to_anchor {
            let step = path.get(end)?;
            if step.from_anchor != anchor || step.to_anchor > shortcut.to_anchor {
                return None;
            }
            anchor = step.to_anchor;
            end += 1;
        }
        if anchor != shortcut.to_anchor {
            return None;
        }
        path.splice(
            start..end,
            [JourneyPathStep {
                from_anchor: shortcut.from_anchor,
                to_anchor: shortcut.to_anchor,
                movement: JourneyPathMovement::BikeShare { shortcut },
            }],
        );
        Some(Self {
            skeleton: baseline.skeleton.clone(),
            path,
        })
    }

    pub fn transit_pattern_sequence(&self) -> Vec<crate::domain::ids::TransitPatternId> {
        self.path
            .iter()
            .filter_map(|step| match &step.movement {
                JourneyPathMovement::Baseline { movement_index } => {
                    match &self.skeleton.movements().get(*movement_index)?.kind {
                        JourneyMovementKind::Transit { trip, .. } => Some(trip.pattern_id.clone()),
                        JourneyMovementKind::Walking { .. } => None,
                    }
                }
                JourneyPathMovement::BikeShare { .. } => None,
            })
            .collect()
    }

    pub(crate) fn realtime_evidence_count(&self) -> usize {
        self.path
            .iter()
            .filter_map(|step| match &step.movement {
                JourneyPathMovement::Baseline { movement_index } => {
                    match &self.skeleton.movements().get(*movement_index)?.kind {
                        JourneyMovementKind::Transit { trip, .. } => Some(trip.evidence.len()),
                        JourneyMovementKind::Walking { .. } => None,
                    }
                }
                JourneyPathMovement::BikeShare { .. } => None,
            })
            .sum()
    }
}

impl JourneySkeleton {
    fn baseline_movement_signature(&self, movement_index: usize) -> String {
        let Some(movement) = self.movements.get(movement_index) else {
            return format!("invalid:{movement_index}");
        };
        let from = anchor_signature(&self.anchors[movement.from_anchor].kind);
        let to = anchor_signature(&self.anchors[movement.to_anchor].kind);
        match &movement.kind {
            JourneyMovementKind::Walking {
                kind,
                duration_seconds,
                distance_metres,
            } => format!("walking:{kind:?}:{from}:{to}:{duration_seconds}:{distance_metres}"),
            JourneyMovementKind::Transit {
                trip,
                board_index,
                alight_index,
            } => {
                let Some(board) = trip.stop_times.get(*board_index) else {
                    return format!("invalid-transit:{board_index}");
                };
                let Some(alight) = trip.stop_times.get(*alight_index) else {
                    return format!("invalid-transit:{alight_index}");
                };
                format!(
                    "transit:{}:{}:{}:{}:{}:{}:{}:{}",
                    trip.id.as_str(),
                    trip.pattern_id.as_str(),
                    board.stop_id.as_str(),
                    alight.stop_id.as_str(),
                    board.departure_at.unix_timestamp_nanos(),
                    alight.arrival_at.unix_timestamp_nanos(),
                    from,
                    to,
                )
            }
        }
    }

    fn baseline_signature(&self) -> String {
        let mut parts = Vec::with_capacity(self.movements.len());
        for movement in &self.movements {
            let from = anchor_signature(&self.anchors[movement.from_anchor].kind);
            let to = anchor_signature(&self.anchors[movement.to_anchor].kind);
            match &movement.kind {
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Access,
                    duration_seconds,
                    ..
                } => parts.push(format!(
                    "access:{}:{}",
                    match &self.anchors[movement.to_anchor].kind {
                        JourneyAnchorKind::BoardingPoint { id } => id.as_str(),
                        _ => "invalid",
                    },
                    duration_seconds
                )),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Transfer,
                    duration_seconds,
                    ..
                } => parts.push(format!(
                    "transfer:{}:{}:{}",
                    from.trim_start_matches("boarding_point:"),
                    to.trim_start_matches("boarding_point:"),
                    duration_seconds
                )),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Egress,
                    duration_seconds,
                    ..
                } => parts.push(format!(
                    "egress:{}:{}",
                    from.trim_start_matches("boarding_point:"),
                    duration_seconds
                )),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Direct,
                    duration_seconds,
                    distance_metres,
                } => parts.push(format!(
                    "direct-walking:{}:{}:{}:{}",
                    self.departure_at.unix_timestamp_nanos(),
                    duration_seconds,
                    distance_metres,
                    to
                )),
                JourneyMovementKind::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => {
                    let Some(board) = trip.stop_times.get(*board_index) else {
                        parts.push(format!("invalid-transit:{board_index}"));
                        continue;
                    };
                    parts.push(format!(
                        "transit:{}:{}:{}:{}",
                        trip.id.as_str(),
                        board_index,
                        alight_index,
                        board.departure_at.unix_timestamp_nanos(),
                    ));
                }
            }
        }
        parts.join("|")
    }
}

fn anchor_signature(kind: &JourneyAnchorKind) -> String {
    match kind {
        JourneyAnchorKind::Origin => "origin".into(),
        JourneyAnchorKind::BoardingPoint { id } => format!("boarding_point:{}", id.as_str()),
        JourneyAnchorKind::Destination => "destination".into(),
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
    BikeShare { shortcut: BikeShortcut },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShortcut {
    pub from_anchor: usize,
    pub to_anchor: usize,
    pub pickup_station_id: BikeStationId,
    pub dropoff_station_id: BikeStationId,
    pub walk_to_pickup: BikeWalkingAccess,
    pub bicycle_duration_seconds: u32,
    pub bicycle_distance_metres: u32,
    pub walk_from_dropoff: BikeWalkingAccess,
    pub total_seconds: u32,
}

impl BikeShortcut {
    pub fn new(
        from_anchor: usize,
        to_anchor: usize,
        pickup_station_id: BikeStationId,
        dropoff_station_id: BikeStationId,
        walk_to_pickup: BikeWalkingAccess,
        bicycle: BikeRide,
        walk_from_dropoff: BikeWalkingAccess,
    ) -> Option<Self> {
        if pickup_station_id == dropoff_station_id || to_anchor <= from_anchor {
            return None;
        }
        let total_seconds = walk_to_pickup
            .duration_seconds
            .checked_add(bicycle.duration_seconds)?
            .checked_add(walk_from_dropoff.duration_seconds)?;
        Some(Self {
            from_anchor,
            to_anchor,
            pickup_station_id,
            dropoff_station_id,
            walk_to_pickup,
            bicycle_duration_seconds: bicycle.duration_seconds,
            bicycle_distance_metres: bicycle.distance_metres,
            walk_from_dropoff,
            total_seconds,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BikeRide {
    pub duration_seconds: u32,
    pub distance_metres: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JourneyVariantTiming {
    pub departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub walking_seconds: u32,
    pub bicycle_seconds: u32,
    pub bicycle_distance_metres: u32,
    pub bike_share_uses: u32,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum JourneyTimingError {
    #[error("journey variant path is not contiguous")]
    NonContiguousPath,
    #[error("journey variant path is incomplete")]
    IncompletePath,
    #[error("journey variant references invalid baseline movement {movement_index}")]
    InvalidBaselineMovement { movement_index: usize },
    #[error("journey variant misses baseline transit movement {movement_index}")]
    MissedTransit { movement_index: usize },
    #[error("journey variant contains an invalid bike-share shortcut")]
    InvalidBikeShortcut,
}

#[cfg(test)]
mod tests {
    use time::{Duration, macros::datetime};

    use crate::{
        domain::{ids::BoardingPointId, transit_timing::TimingReliability},
        routing::{AccessPoint, EgressPoint, JourneyCandidate},
    };

    use super::*;

    #[test]
    fn shortcut_replaces_a_contiguous_interval_of_baseline_movements() {
        let departure_at = datetime!(2026-09-21 10:00 UTC);
        let a = BoardingPointId::new("a");
        let b = BoardingPointId::new("b");
        let c = BoardingPointId::new("c");
        let candidate = JourneyCandidate {
            access: AccessPoint::new(a.clone(), departure_at, departure_at, 0, 0),
            egress: EgressPoint::new(c.clone(), 0, 0),
            segments: vec![
                JourneySegment::WalkingTransfer {
                    shortcut: crate::routing::transfers::TransferShortcut::new(
                        a.clone(),
                        b.clone(),
                        60,
                    ),
                },
                JourneySegment::WalkingTransfer {
                    shortcut: crate::routing::transfers::TransferShortcut::new(b, c, 60),
                },
            ],
            departure_time: departure_at,
            arrival_time: departure_at + Duration::seconds(120),
            walking_seconds: 120,
            transfer_count: 0,
            uncertainty_seconds: None,
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
        };
        let baseline =
            JourneyVariant::baseline(JourneySkeleton::from_candidate(candidate).unwrap());
        let shortcut = BikeShortcut::new(
            1,
            3,
            BikeStationId::new("pickup"),
            BikeStationId::new("dropoff"),
            BikeWalkingAccess::new(0, 0),
            BikeRide {
                duration_seconds: 30,
                distance_metres: 100,
            },
            BikeWalkingAccess::new(0, 0),
        )
        .unwrap();

        let variant = JourneyVariant::with_shortcut(&baseline, shortcut).unwrap();

        assert_eq!(variant.path().len(), 3);
        assert!(variant.path().iter().any(|step| {
            step.from_anchor == 1
                && step.to_anchor == 3
                && matches!(step.movement, JourneyPathMovement::BikeShare { .. })
        }));
        assert_eq!(
            variant.evaluate_timing().unwrap().arrival_at,
            departure_at + Duration::seconds(30)
        );
    }
}
