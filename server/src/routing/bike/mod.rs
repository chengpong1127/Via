mod candidates;
mod realization;
mod session;
#[cfg(test)]
mod tests;

use candidates::StationChoice;
pub(crate) use session::BikeShareSession;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use time::OffsetDateTime;

use crate::{
    domain::bikeshare::{
        BikeAvailabilitySnapshot, BikeShareNetwork, BikeStationAccess, BikeWalkingAccess,
    },
    domain::ids::BikeStationId,
    domain::location::GeoCoordinate,
    street::{
        Coordinate, MatrixCell, MatrixLimits, MatrixOrientation, MatrixQuery, MatrixResult,
        RoutingConstraints, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

use super::cache::RequestCache;

use super::{
    corridor::{CorridorAnchor, CorridorAnchorKey},
    realized::{BikeRide, BikeShareMovement},
};

#[derive(Clone, Debug)]
pub struct BikeMovementPolicy {
    pub max_pickup_stations_per_anchor: usize,
    pub max_dropoff_stations_per_anchor: usize,
    pub max_stations_per_endpoint: usize,
    pub endpoint_radius_metres: u32,
    pub max_endpoint_walk_seconds: u32,
    pub max_bike_duration_seconds: u32,
    pub max_bike_distance_metres: u32,
    pub optimistic_walk_speed_metres_per_second: f64,
    pub availability_max_age_seconds: u32,
    pub availability_max_clock_skew_seconds: u32,
}

#[async_trait::async_trait]
pub(crate) trait BikeMovementSource: Send + Sync {
    async fn movements(
        &self,
        from: &CorridorAnchor,
        to: &CorridorAnchor,
        from_anchor: usize,
        to_anchor: usize,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError>;
}

#[derive(Clone)]
pub(crate) struct BikeShareMovementPlanner {
    settings: BikeMovementPolicy,
    network: Arc<BikeShareNetwork>,
}

#[derive(Clone)]
pub(crate) struct BikeMovementCandidate {
    pickups: Vec<StationChoice>,
    dropoffs: Vec<StationChoice>,
    pickup_endpoint: Option<Coordinate>,
    dropoff_endpoint: Option<Coordinate>,
}

impl BikeShareMovementPlanner {
    pub(crate) fn new(network: Arc<BikeShareNetwork>, settings: BikeMovementPolicy) -> Self {
        Self { network, settings }
    }
}
