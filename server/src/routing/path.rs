use std::sync::Arc;

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::{
    domain::{
        bikeshare::BikeWalkingAccess,
        ids::{BikeStationId, BoardingPointId, EffectiveTripId, TransitPatternId},
        transit_timing::EffectiveTrip,
    },
    street::StreetPath,
};

use super::{JourneySegment, RealizedJourney};

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
    anchors: Vec<JourneyAnchor>,
    movements: Vec<JourneyMovement>,
    departure_at: OffsetDateTime,
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

#[derive(Clone, Debug, PartialEq)]
pub struct JourneyVariant {
    skeleton: Arc<JourneySkeleton>,
    path: Vec<JourneyPathStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct JourneyVariantIdentity {
    is_baseline: bool,
    movements: Vec<JourneyVariantMovementIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum JourneyVariantMovementIdentity {
    Baseline(Box<JourneyVariantMovementIdentity>),
    Access {
        stop_id: Option<BoardingPointId>,
        duration_seconds: u32,
    },
    Transfer {
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
        duration_seconds: u32,
    },
    Egress {
        from: JourneyAnchorIdentity,
        duration_seconds: u32,
    },
    DirectWalking {
        departure_unix_nanos: i128,
        duration_seconds: u32,
        distance_metres: u32,
        to: JourneyAnchorIdentity,
    },
    Walking {
        kind: WalkingMovementKind,
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
        duration_seconds: u32,
        distance_metres: u32,
    },
    Transit {
        trip_id: EffectiveTripId,
        board_index: usize,
        alight_index: usize,
        departure_unix_nanos: i128,
    },
    TransitMovement {
        trip_id: EffectiveTripId,
        pattern_id: TransitPatternId,
        board_stop_id: BoardingPointId,
        alight_stop_id: BoardingPointId,
        departure_unix_nanos: i128,
        arrival_unix_nanos: i128,
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
    },
    BikeShare {
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
        pickup_station_id: BikeStationId,
        dropoff_station_id: BikeStationId,
        walk_to_pickup_seconds: u32,
        walk_to_pickup_metres: u32,
        bicycle_duration_seconds: u32,
        bicycle_distance_metres: u32,
        walk_from_dropoff_seconds: u32,
        walk_from_dropoff_metres: u32,
        rental_wait_seconds: u32,
        total_seconds: u32,
    },
    InvalidMovement(usize),
    InvalidTransit(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum JourneyAnchorIdentity {
    Origin,
    BoardingPoint(BoardingPointId),
    Destination,
}

impl JourneyAnchorIdentity {
    fn from_kind(kind: &JourneyAnchorKind) -> Self {
        match kind {
            JourneyAnchorKind::Origin => Self::Origin,
            JourneyAnchorKind::BoardingPoint { id } => Self::BoardingPoint(id.clone()),
            JourneyAnchorKind::Destination => Self::Destination,
        }
    }

    fn legacy_signature(&self) -> String {
        match self {
            Self::Origin => "origin".to_owned(),
            Self::BoardingPoint(id) => format!("boarding_point:{}", id.as_str()),
            Self::Destination => "destination".to_owned(),
        }
    }

    fn boarding_point_id(&self) -> Option<&BoardingPointId> {
        match self {
            Self::BoardingPoint(id) => Some(id),
            Self::Origin | Self::Destination => None,
        }
    }

    fn legacy_unprefixed(&self) -> &str {
        match self {
            Self::Origin => "origin",
            Self::BoardingPoint(id) => id.as_str().trim_start_matches("boarding_point:"),
            Self::Destination => "destination",
        }
    }
}

impl JourneyVariantIdentity {
    pub(crate) fn canonical_signature(&self) -> String {
        let parts = self
            .movements
            .iter()
            .map(JourneyVariantMovementIdentity::legacy_signature)
            .collect::<Vec<_>>()
            .join("|");
        if self.is_baseline {
            parts
        } else {
            format!("path:v2:{parts}")
        }
    }
}

impl JourneyVariantMovementIdentity {
    fn legacy_signature(&self) -> String {
        match self {
            Self::Baseline(movement) => format!("baseline:{}", movement.legacy_signature()),
            Self::Access {
                stop_id,
                duration_seconds,
            } => format!(
                "access:{}:{duration_seconds}",
                stop_id.as_ref().map_or("invalid", BoardingPointId::as_str)
            ),
            Self::Transfer {
                from,
                to,
                duration_seconds,
            } => format!(
                "transfer:{}:{}:{duration_seconds}",
                from.legacy_unprefixed(),
                to.legacy_unprefixed()
            ),
            Self::Egress {
                from,
                duration_seconds,
            } => format!("egress:{}:{duration_seconds}", from.legacy_unprefixed()),
            Self::DirectWalking {
                departure_unix_nanos,
                duration_seconds,
                distance_metres,
                to,
            } => format!(
                "direct-walking:{departure_unix_nanos}:{duration_seconds}:{distance_metres}:{}",
                to.legacy_signature()
            ),
            Self::Walking {
                kind,
                from,
                to,
                duration_seconds,
                distance_metres,
            } => format!(
                "walking:{kind:?}:{}:{}:{duration_seconds}:{distance_metres}",
                from.legacy_signature(),
                to.legacy_signature()
            ),
            Self::Transit {
                trip_id,
                board_index,
                alight_index,
                departure_unix_nanos,
            } => format!(
                "transit:{}:{board_index}:{alight_index}:{departure_unix_nanos}",
                trip_id.as_str()
            ),
            Self::TransitMovement {
                trip_id,
                pattern_id,
                board_stop_id,
                alight_stop_id,
                departure_unix_nanos,
                arrival_unix_nanos,
                from,
                to,
            } => format!(
                "transit:{}:{}:{}:{}:{departure_unix_nanos}:{arrival_unix_nanos}:{}:{}",
                trip_id.as_str(),
                pattern_id.as_str(),
                board_stop_id.as_str(),
                alight_stop_id.as_str(),
                from.legacy_signature(),
                to.legacy_signature()
            ),
            Self::BikeShare {
                from,
                to,
                pickup_station_id,
                dropoff_station_id,
                walk_to_pickup_seconds,
                walk_to_pickup_metres,
                bicycle_duration_seconds,
                bicycle_distance_metres,
                walk_from_dropoff_seconds,
                walk_from_dropoff_metres,
                rental_wait_seconds,
                total_seconds,
            } => format!(
                "bike:{}:{}:{}:{}:{walk_to_pickup_seconds}:{walk_to_pickup_metres}:{bicycle_duration_seconds}:{bicycle_distance_metres}:{walk_from_dropoff_seconds}:{walk_from_dropoff_metres}:{rental_wait_seconds}:{total_seconds}",
                from.legacy_signature(),
                to.legacy_signature(),
                pickup_station_id.as_str(),
                dropoff_station_id.as_str()
            ),
            Self::InvalidMovement(index) => format!("invalid:{index}"),
            Self::InvalidTransit(index) => format!("invalid-transit:{index}"),
        }
    }
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

    pub(crate) fn identity(&self) -> JourneyVariantIdentity {
        let is_baseline = self.is_baseline();
        let movements = if is_baseline {
            self.skeleton.baseline_identity()
        } else {
            self.path
                .iter()
                .map(|step| match &step.movement {
                    JourneyPathMovement::Baseline { movement_index } => {
                        JourneyVariantMovementIdentity::Baseline(Box::new(
                            self.skeleton.baseline_movement_identity(*movement_index),
                        ))
                    }
                    JourneyPathMovement::BikeShare { shortcut } => {
                        JourneyVariantMovementIdentity::BikeShare {
                            from: JourneyAnchorIdentity::from_kind(
                                &self.skeleton.anchors()[step.from_anchor].kind,
                            ),
                            to: JourneyAnchorIdentity::from_kind(
                                &self.skeleton.anchors()[step.to_anchor].kind,
                            ),
                            pickup_station_id: shortcut.pickup_station_id.clone(),
                            dropoff_station_id: shortcut.dropoff_station_id.clone(),
                            walk_to_pickup_seconds: shortcut.walk_to_pickup.duration_seconds,
                            walk_to_pickup_metres: shortcut.walk_to_pickup.distance_metres,
                            bicycle_duration_seconds: shortcut.bicycle_duration_seconds,
                            bicycle_distance_metres: shortcut.bicycle_distance_metres,
                            walk_from_dropoff_seconds: shortcut.walk_from_dropoff.duration_seconds,
                            walk_from_dropoff_metres: shortcut.walk_from_dropoff.distance_metres,
                            rental_wait_seconds: shortcut.rental_wait_seconds,
                            total_seconds: shortcut.total_seconds,
                        }
                    }
                })
                .collect()
        };
        JourneyVariantIdentity {
            is_baseline,
            movements,
        }
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
    fn baseline_movement_identity(&self, movement_index: usize) -> JourneyVariantMovementIdentity {
        let Some(movement) = self.movements.get(movement_index) else {
            return JourneyVariantMovementIdentity::InvalidMovement(movement_index);
        };
        let from = JourneyAnchorIdentity::from_kind(&self.anchors[movement.from_anchor].kind);
        let to = JourneyAnchorIdentity::from_kind(&self.anchors[movement.to_anchor].kind);
        match &movement.kind {
            JourneyMovementKind::Walking {
                kind,
                duration_seconds,
                distance_metres,
            } => JourneyVariantMovementIdentity::Walking {
                kind: *kind,
                from,
                to,
                duration_seconds: *duration_seconds,
                distance_metres: *distance_metres,
            },
            JourneyMovementKind::Transit {
                trip,
                board_index,
                alight_index,
            } => {
                let Some(board) = trip.stop_times.get(*board_index) else {
                    return JourneyVariantMovementIdentity::InvalidTransit(*board_index);
                };
                let Some(alight) = trip.stop_times.get(*alight_index) else {
                    return JourneyVariantMovementIdentity::InvalidTransit(*alight_index);
                };
                JourneyVariantMovementIdentity::TransitMovement {
                    trip_id: trip.id.clone(),
                    pattern_id: trip.pattern_id.clone(),
                    board_stop_id: board.stop_id.clone(),
                    alight_stop_id: alight.stop_id.clone(),
                    departure_unix_nanos: board.departure_at.unix_timestamp_nanos(),
                    arrival_unix_nanos: alight.arrival_at.unix_timestamp_nanos(),
                    from,
                    to,
                }
            }
        }
    }

    fn baseline_identity(&self) -> Vec<JourneyVariantMovementIdentity> {
        let mut identities = Vec::with_capacity(self.movements.len());
        for movement in &self.movements {
            let from = JourneyAnchorIdentity::from_kind(&self.anchors[movement.from_anchor].kind);
            let to = JourneyAnchorIdentity::from_kind(&self.anchors[movement.to_anchor].kind);
            match &movement.kind {
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Access,
                    duration_seconds,
                    ..
                } => identities.push(JourneyVariantMovementIdentity::Access {
                    stop_id: to.boarding_point_id().cloned(),
                    duration_seconds: *duration_seconds,
                }),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Transfer,
                    duration_seconds,
                    ..
                } => identities.push(JourneyVariantMovementIdentity::Transfer {
                    from,
                    to,
                    duration_seconds: *duration_seconds,
                }),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Egress,
                    duration_seconds,
                    ..
                } => identities.push(JourneyVariantMovementIdentity::Egress {
                    from,
                    duration_seconds: *duration_seconds,
                }),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Direct,
                    duration_seconds,
                    distance_metres,
                } => identities.push(JourneyVariantMovementIdentity::DirectWalking {
                    departure_unix_nanos: self.departure_at.unix_timestamp_nanos(),
                    duration_seconds: *duration_seconds,
                    distance_metres: *distance_metres,
                    to,
                }),
                JourneyMovementKind::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => {
                    let Some(board) = trip.stop_times.get(*board_index) else {
                        identities
                            .push(JourneyVariantMovementIdentity::InvalidTransit(*board_index));
                        continue;
                    };
                    identities.push(JourneyVariantMovementIdentity::Transit {
                        trip_id: trip.id.clone(),
                        board_index: *board_index,
                        alight_index: *alight_index,
                        departure_unix_nanos: board.departure_at.unix_timestamp_nanos(),
                    });
                }
            }
        }
        identities
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
    pub rental_wait_seconds: u32,
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
            rental_wait_seconds: 0,
            total_seconds,
        })
    }

    pub(crate) fn with_rental_wait(mut self, seconds: u32) -> Option<Self> {
        let route_seconds = self.total_seconds.checked_sub(self.rental_wait_seconds)?;
        self.total_seconds = route_seconds.checked_add(seconds)?;
        self.rental_wait_seconds = seconds;
        Some(self)
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
        routing::{AccessPoint, EgressPoint, RealizedJourney},
    };

    use super::*;

    #[test]
    fn shortcut_replaces_a_contiguous_interval_of_baseline_movements() {
        let departure_at = datetime!(2026-09-21 10:00 UTC);
        let a = BoardingPointId::new("a");
        let b = BoardingPointId::new("b");
        let c = BoardingPointId::new("c");
        let candidate = RealizedJourney {
            access: AccessPoint::new(a.clone(), departure_at, departure_at, 0, 0),
            egress: EgressPoint::new(c.clone(), 0, 0),
            segments: vec![
                JourneySegment::WalkingTransfer {
                    shortcut: crate::routing::transfers::TransferShortcut::new(
                        a.clone(),
                        b.clone(),
                        60,
                        100,
                    ),
                },
                JourneySegment::WalkingTransfer {
                    shortcut: crate::routing::transfers::TransferShortcut::new(b, c, 60, 100),
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
        assert_eq!(
            baseline.identity().canonical_signature(),
            "access:a:0|transfer:a:b:60|transfer:b:c:60|egress:c:0"
        );
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

        assert_eq!(
            variant.identity().canonical_signature(),
            "path:v2:baseline:walking:Access:origin:boarding_point:a:0:0|bike:boarding_point:a:boarding_point:c:pickup:dropoff:0:0:30:100:0:0:0:30|baseline:walking:Egress:boarding_point:c:destination:0:0"
        );

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
