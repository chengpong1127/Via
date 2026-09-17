//! Query-time BikeShare connectors and availability.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::domain::{
    ids::BikeShareStationId,
    realtime::{ActionState, BikeAvailability},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointLeg {
    Walk {
        duration_seconds: u32,
        distance_metres: u32,
    },
    BikeShare {
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        duration_seconds: u32,
        distance_metres: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndpointPath {
    pub legs: Vec<EndpointLeg>,
}

impl EndpointPath {
    pub fn walking(duration_seconds: u32, distance_metres: u32) -> Self {
        Self {
            legs: vec![EndpointLeg::Walk {
                duration_seconds,
                distance_metres,
            }],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn walking_and_bike(
        access_seconds: u32,
        access_metres: u32,
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        bike_seconds: u32,
        bike_metres: u32,
        egress_seconds: u32,
        egress_metres: u32,
    ) -> Self {
        Self {
            legs: vec![
                EndpointLeg::Walk {
                    duration_seconds: access_seconds,
                    distance_metres: access_metres,
                },
                EndpointLeg::BikeShare {
                    pickup_station_id,
                    return_station_id,
                    duration_seconds: bike_seconds,
                    distance_metres: bike_metres,
                },
                EndpointLeg::Walk {
                    duration_seconds: egress_seconds,
                    distance_metres: egress_metres,
                },
            ],
        }
    }

    pub fn duration_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                EndpointLeg::Walk {
                    duration_seconds, ..
                }
                | EndpointLeg::BikeShare {
                    duration_seconds, ..
                } => *duration_seconds,
            })
        })
    }

    pub fn walking_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                EndpointLeg::Walk {
                    duration_seconds, ..
                } => *duration_seconds,
                EndpointLeg::BikeShare { .. } => 0,
            })
        })
    }

    pub fn bike_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                EndpointLeg::BikeShare {
                    duration_seconds, ..
                } => *duration_seconds,
                EndpointLeg::Walk { .. } => 0,
            })
        })
    }

    pub fn required_station_ids(&self) -> Vec<BikeShareStationId> {
        let mut ids = self
            .legs
            .iter()
            .flat_map(|leg| match leg {
                EndpointLeg::BikeShare {
                    pickup_station_id,
                    return_station_id,
                    ..
                } => vec![pickup_station_id.clone(), return_station_id.clone()],
                EndpointLeg::Walk { .. } => Vec::new(),
            })
            .collect::<Vec<_>>();
        ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ids.dedup();
        ids
    }

    pub fn uses_bike_share(&self) -> bool {
        self.legs
            .iter()
            .any(|leg| matches!(leg, EndpointLeg::BikeShare { .. }))
    }
}

#[async_trait]
pub trait BikeAvailabilityResolver: Send + Sync {
    async fn availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<HashMap<BikeShareStationId, BikeAvailability>, BikeAvailabilityError>;
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BikeAvailabilityError {
    #[error("bike-share availability is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("bike-share availability response is invalid")]
    InvalidResponse,
}

pub fn bike_availability_is_usable(
    pickup: Option<&BikeAvailability>,
    return_station: Option<&BikeAvailability>,
) -> bool {
    pickup.is_none_or(|availability| {
        availability.rental_state != ActionState::Disabled
            && availability.available_bikes != Some(0)
    }) && return_station.is_none_or(|availability| {
        availability.return_state != ActionState::Disabled
            && availability.available_return_slots != Some(0)
    })
}
