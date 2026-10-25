mod identity;
mod movement;
#[cfg(test)]
mod tests;

use crate::domain::{bikeshare::BikeWalkingAccess, ids::BikeStationId};
pub use movement::{
    JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind, RealizedJourneyError,
    WalkingMovementKind,
};
use std::sync::Arc;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug, PartialEq)]
pub struct RealizedJourney {
    anchors: Vec<JourneyAnchor>,
    movements: Vec<JourneyMovement>,
    departure_at: OffsetDateTime,
}

impl RealizedJourney {
    pub fn anchors(&self) -> &[JourneyAnchor] {
        &self.anchors
    }
    pub fn movements(&self) -> &[JourneyMovement] {
        &self.movements
    }
    pub fn departure_at(&self) -> OffsetDateTime {
        self.departure_at
    }

    pub fn from_movements(
        anchors: Vec<JourneyAnchor>,
        movements: Vec<JourneyMovement>,
        departure_at: OffsetDateTime,
    ) -> Result<Self, RealizedJourneyError> {
        if anchors.len() < 2 || movements.is_empty() {
            return Err(RealizedJourneyError::NoMovements);
        }
        if anchors[0].reached_at != departure_at
            || movements.iter().any(|movement| {
                movement.from_anchor >= anchors.len()
                    || movement.to_anchor >= anchors.len()
                    || movement.arrival_at < movement.departure_at
            })
        {
            return Err(RealizedJourneyError::InvalidRealizedPath);
        }
        let journey = Self {
            anchors,
            movements,
            departure_at,
        };
        journey
            .evaluate_timing()
            .map_err(|_| RealizedJourneyError::InvalidRealizedPath)?;
        Ok(journey)
    }

    pub fn has_bike_share(&self) -> bool {
        self.movements
            .iter()
            .any(|movement| matches!(movement.kind, JourneyMovementKind::BikeShare { .. }))
    }

    pub fn evaluate_timing(&self) -> Result<JourneyTiming, JourneyTimingError> {
        let mut current_anchor = 0;
        let mut current_time = self.departure_at;
        let mut timing = JourneyTiming {
            departure_at: current_time,
            arrival_at: current_time,
            walking_seconds: 0,
            bicycle_seconds: 0,
            bicycle_distance_metres: 0,
            bike_share_uses: 0,
        };
        for (movement_index, movement) in self.movements.iter().enumerate() {
            if movement.from_anchor != current_anchor || movement.to_anchor <= movement.from_anchor
            {
                return Err(JourneyTimingError::NonContiguousPath);
            }
            match &movement.kind {
                JourneyMovementKind::Walking {
                    duration_seconds, ..
                } => {
                    current_time += Duration::seconds(i64::from(*duration_seconds));
                    timing.walking_seconds =
                        timing.walking_seconds.saturating_add(*duration_seconds);
                }
                JourneyMovementKind::Transit { .. } => {
                    if current_time > movement.departure_at {
                        return Err(JourneyTimingError::MissedTransit { movement_index });
                    }
                    current_time = movement.arrival_at;
                }
                JourneyMovementKind::BikeShare {
                    movement: bike_movement,
                } => {
                    if bike_movement.from_anchor != movement.from_anchor
                        || bike_movement.to_anchor != movement.to_anchor
                    {
                        return Err(JourneyTimingError::InvalidBikeShareMovement);
                    }
                    current_time += Duration::seconds(i64::from(bike_movement.total_seconds));
                    timing.walking_seconds = timing
                        .walking_seconds
                        .saturating_add(bike_movement.walk_to_pickup.duration_seconds)
                        .saturating_add(bike_movement.walk_from_dropoff.duration_seconds);
                    timing.bicycle_seconds = timing
                        .bicycle_seconds
                        .saturating_add(bike_movement.bicycle_duration_seconds);
                    timing.bicycle_distance_metres = timing
                        .bicycle_distance_metres
                        .saturating_add(bike_movement.bicycle_distance_metres);
                    timing.bike_share_uses = timing.bike_share_uses.saturating_add(1);
                }
            }
            current_anchor = movement.to_anchor;
        }
        if current_anchor != self.anchors.len().saturating_sub(1) {
            return Err(JourneyTimingError::IncompletePath);
        }
        timing.arrival_at = current_time;
        Ok(timing)
    }
    pub fn transit_pattern_sequence(&self) -> Vec<crate::domain::ids::TransitPatternId> {
        self.movements
            .iter()
            .filter_map(|movement| match &movement.kind {
                JourneyMovementKind::Transit { trip, .. } => Some(trip.pattern_id.clone()),
                _ => None,
            })
            .collect()
    }
    pub(super) fn realtime_evidence_count(&self) -> usize {
        self.movements
            .iter()
            .map(|movement| match &movement.kind {
                JourneyMovementKind::Transit { trip, .. } => trip.evidence.len(),
                _ => 0,
            })
            .sum()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareMovement {
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

impl BikeShareMovement {
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
pub struct JourneyTiming {
    pub departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub walking_seconds: u32,
    pub bicycle_seconds: u32,
    pub bicycle_distance_metres: u32,
    pub bike_share_uses: u32,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum JourneyTimingError {
    #[error("realized journey path is not contiguous")]
    NonContiguousPath,
    #[error("realized journey path is incomplete")]
    IncompletePath,
    #[error("realized journey misses realized transit movement {movement_index}")]
    MissedTransit { movement_index: usize },
    #[error("realized journey contains an invalid BikeShare movement")]
    InvalidBikeShareMovement,
}
