mod identity;
mod skeleton;
#[cfg(test)]
mod tests;

pub(in crate::routing) use identity::JourneyVariantIdentity;
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

    pub(super) fn with_shortcut(baseline: &Self, shortcut: BikeShortcut) -> Option<Self> {
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

    pub(super) fn realtime_evidence_count(&self) -> usize {
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
    #[error("journey variant references invalid baseline movement {movement_index}")]
    InvalidBaselineMovement { movement_index: usize },
    #[error("journey variant misses baseline transit movement {movement_index}")]
    MissedTransit { movement_index: usize },
    #[error("journey variant contains an invalid bike-share shortcut")]
    InvalidBikeShortcut,
}
