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
        BikeAvailabilitySnapshot, BikeAvailabilityView, BikeShareNetwork, BikeStationAccess,
        BikeWalkingAccess,
    },
    domain::ids::{BikeStationId, BoardingPointId},
    domain::location::GeoCoordinate,
    street::{
        Coordinate, MatrixCell, MatrixLimits, MatrixOrientation, MatrixQuery, MatrixResult,
        RoutingConstraints, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

use crate::routing::corridors::{CorridorAnchor, CorridorAnchorKey};
use crate::routing::{
    cache::RequestCache,
    journey::{BikeRide, BikeShareMovement},
};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BikeMovementPolicy {
    pub max_pickup_stations_per_anchor: usize,
    pub max_dropoff_stations_per_anchor: usize,
    pub max_stations_per_endpoint: usize,
    pub endpoint_radius_metres: u32,
    pub max_endpoint_walk_seconds: u32,
    pub max_bike_duration_seconds: u32,
    pub max_bike_distance_metres: u32,
    pub optimistic_walk_speed_metres_per_second: f64,
    pub rental_handling_seconds: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BikeAvailabilityLimits {
    pub max_age_seconds: u32,
    pub max_clock_skew_seconds: u32,
}

#[async_trait::async_trait]
pub(crate) trait BikeMovementSource: Send + Sync {
    async fn movements(
        &self,
        from: &CorridorAnchor,
        to: &CorridorAnchor,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StationRole {
    Pickup,
    Dropoff,
}

impl StationRole {
    fn walking(self, access: &BikeStationAccess) -> Option<BikeWalkingAccess> {
        match self {
            Self::Pickup => access.walk_to_station,
            Self::Dropoff => access.walk_from_station,
        }
    }

    fn max_per_anchor(self, settings: &BikeMovementPolicy) -> usize {
        match self {
            Self::Pickup => settings.max_pickup_stations_per_anchor,
            Self::Dropoff => settings.max_dropoff_stations_per_anchor,
        }
    }

    fn usable_accesses<'a>(
        self,
        view: &BikeAvailabilityView<'a>,
        boarding_point_id: &BoardingPointId,
    ) -> Vec<&'a BikeStationAccess> {
        match self {
            Self::Pickup => view.usable_pickups(boarding_point_id),
            Self::Dropoff => view.usable_dropoffs(boarding_point_id),
        }
    }

    fn is_available(self, view: &BikeAvailabilityView<'_>, station_id: &BikeStationId) -> bool {
        match self {
            Self::Pickup => view.can_pick_up_at(station_id),
            Self::Dropoff => view.can_return_at(station_id),
        }
    }

    fn walking_orientation(self) -> MatrixOrientation {
        match self {
            Self::Pickup => MatrixOrientation::OneToMany,
            Self::Dropoff => MatrixOrientation::ManyToOne,
        }
    }

    fn chunk_size(self, limits: MatrixLimits) -> usize {
        match self {
            Self::Pickup => limits.max_targets,
            Self::Dropoff => limits.max_sources,
        }
    }

    fn walking_endpoints(
        self,
        endpoint: Coordinate,
        stations: Vec<Coordinate>,
    ) -> (Vec<Coordinate>, Vec<Coordinate>) {
        match self {
            Self::Pickup => (vec![endpoint], stations),
            Self::Dropoff => (stations, vec![endpoint]),
        }
    }

    fn walking_shape(self, stations: usize) -> (usize, usize) {
        match self {
            Self::Pickup => (1, stations),
            Self::Dropoff => (stations, 1),
        }
    }

    fn walking_cell(self, matrix: &MatrixResult, station: usize) -> &MatrixCell {
        match self {
            Self::Pickup => &matrix.cells[0][station],
            Self::Dropoff => &matrix.cells[station][0],
        }
    }
}

#[derive(Clone)]
pub(crate) struct BikeShareMovementPlanner {
    settings: BikeMovementPolicy,
    availability: BikeAvailabilityLimits,
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
    pub(crate) fn new(
        network: Arc<BikeShareNetwork>,
        settings: BikeMovementPolicy,
        availability: BikeAvailabilityLimits,
    ) -> Self {
        Self {
            network,
            settings,
            availability,
        }
    }
}
