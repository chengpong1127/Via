mod identity;
mod skeleton;
#[cfg(test)]
mod tests;

pub use skeleton::{
    JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind, JourneySkeleton,
    JourneySkeletonError, WalkingMovementKind,
};

use std::sync::Arc;

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::domain::{bikeshare::BikeWalkingAccess, ids::BikeStationId};

#[derive(Clone, Debug, PartialEq)]
pub struct JourneyVariant {
    pub(super) skeleton: Arc<JourneySkeleton>,
    pub(super) path: Vec<JourneyPathStep>,
}

impl JourneyVariant {
    fn from_skeleton(skeleton: JourneySkeleton) -> Self {
        let path = skeleton
            .movements()
            .iter()
            .enumerate()
            .map(|(movement_index, movement)| JourneyPathStep {
                from_anchor: movement.from_anchor,
                to_anchor: movement.to_anchor,
                movement: match &movement.kind {
                    JourneyMovementKind::Walking { .. } => {
                        JourneyPathMovement::Walking { movement_index }
                    }
                    JourneyMovementKind::Transit { .. } => {
                        JourneyPathMovement::Transit { movement_index }
                    }
                    JourneyMovementKind::BikeShare { shortcut } => JourneyPathMovement::BikeShare {
                        shortcut: shortcut.clone(),
                    },
                },
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

    /// Construct a path whose movements already contain the evaluated timing and geometry.
    pub fn from_movements(
        anchors: Vec<JourneyAnchor>,
        movements: Vec<JourneyMovement>,
        departure_at: OffsetDateTime,
    ) -> Result<Self, JourneySkeletonError> {
        if anchors.len() < 2 || movements.is_empty() {
            return Err(JourneySkeletonError::NoMovements);
        }
        if anchors[0].reached_at != departure_at
            || movements.iter().any(|movement| {
                movement.from_anchor >= anchors.len()
                    || movement.to_anchor >= anchors.len()
                    || movement.arrival_at < movement.departure_at
                    || (movement.street_path.is_some()
                        && !matches!(movement.kind, JourneyMovementKind::Walking { .. }))
            })
        {
            return Err(JourneySkeletonError::InvalidRealizedPath);
        }
        let variant = Self::from_skeleton(JourneySkeleton {
            anchors,
            movements,
            departure_at,
        });
        variant
            .evaluate_timing()
            .map_err(|_| JourneySkeletonError::InvalidRealizedPath)?;
        Ok(variant)
    }

    pub fn is_baseline(&self) -> bool {
        !self
            .path
            .iter()
            .any(|step| matches!(step.movement, JourneyPathMovement::BikeShare { .. }))
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
                JourneyPathMovement::Walking { movement_index }
                | JourneyPathMovement::Transit { movement_index } => {
                    let movement = self.skeleton.movements().get(*movement_index).ok_or(
                        JourneyTimingError::InvalidRealizedMovement {
                            movement_index: *movement_index,
                        },
                    )?;
                    if movement.from_anchor != step.from_anchor
                        || movement.to_anchor != step.to_anchor
                    {
                        return Err(JourneyTimingError::InvalidRealizedMovement {
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
                        JourneyMovementKind::BikeShare { .. } => {
                            return Err(JourneyTimingError::InvalidBikeShortcut);
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

    pub fn transit_pattern_sequence(&self) -> Vec<crate::domain::ids::TransitPatternId> {
        self.path
            .iter()
            .filter_map(|step| match &step.movement {
                JourneyPathMovement::Walking { movement_index }
                | JourneyPathMovement::Transit { movement_index } => {
                    match &self.skeleton.movements().get(*movement_index)?.kind {
                        JourneyMovementKind::Transit { trip, .. } => Some(trip.pattern_id.clone()),
                        JourneyMovementKind::Walking { .. }
                        | JourneyMovementKind::BikeShare { .. } => None,
                    }
                }
                JourneyPathMovement::BikeShare { .. } => None,
            })
            .collect()
    }

    pub(super) fn realtime_evidence_count(&self) -> usize {
        self.path
            .iter()
            .filter_map(|step| match &step.movement {
                JourneyPathMovement::Walking { movement_index }
                | JourneyPathMovement::Transit { movement_index } => {
                    match &self.skeleton.movements().get(*movement_index)?.kind {
                        JourneyMovementKind::Transit { trip, .. } => Some(trip.evidence.len()),
                        JourneyMovementKind::Walking { .. }
                        | JourneyMovementKind::BikeShare { .. } => None,
                    }
                }
                JourneyPathMovement::BikeShare { .. } => None,
            })
            .sum()
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
    Walking { movement_index: usize },
    Transit { movement_index: usize },
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

    pub(super) fn with_rental_wait(mut self, seconds: u32) -> Option<Self> {
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
    #[error("journey variant references invalid realized movement {movement_index}")]
    InvalidRealizedMovement { movement_index: usize },
    #[error("journey variant misses realized transit movement {movement_index}")]
    MissedTransit { movement_index: usize },
    #[error("journey variant contains an invalid bike-share shortcut")]
    InvalidBikeShortcut,
}
