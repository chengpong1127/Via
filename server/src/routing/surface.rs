use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use time::OffsetDateTime;

use super::realized::{BikeShareMovement, JourneyAnchorKind};
use crate::{
    domain::{ids::BoardingPointId, location::GeoCoordinate, normalize_to_taipei},
    street::{
        Coordinate, RouteQuery, RoutingConstraints, RoutingTime, RoutingTimeKind, StreetPath,
        StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CorridorAnchor {
    pub kind: JourneyAnchorKind,
    pub coordinate: Coordinate,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CorridorAnchorKey {
    latitude: u64,
    longitude: u64,
    boarding_point: Option<BoardingPointId>,
}

impl CorridorAnchor {
    pub fn boarding_point(&self) -> Option<&BoardingPointId> {
        match &self.kind {
            JourneyAnchorKind::BoardingPoint { id } => Some(id),
            _ => None,
        }
    }
    pub fn cache_key(&self) -> CorridorAnchorKey {
        CorridorAnchorKey {
            latitude: self.coordinate.latitude().to_bits(),
            longitude: self.coordinate.longitude().to_bits(),
            boarding_point: self.boarding_point().cloned(),
        }
    }
    pub fn distance_to(&self, other: &Self) -> f64 {
        GeoCoordinate {
            latitude: self.coordinate.latitude(),
            longitude: self.coordinate.longitude(),
        }
        .distance_metres(GeoCoordinate {
            latitude: other.coordinate.latitude(),
            longitude: other.coordinate.longitude(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceKind {
    Walking,
    BikeShare,
}

pub(crate) struct SurfaceRequest<'a> {
    pub from: &'a CorridorAnchor,
    pub to: &'a CorridorAnchor,
    pub departure_at: OffsetDateTime,
    pub from_anchor: usize,
    pub to_anchor: usize,
}

pub(crate) enum SurfaceMovement {
    Walking(Arc<StreetPath>),
    BikeShare(BikeShareMovement),
}

/// Cumulative request-local exact routing work, including failed requests.
#[derive(Clone, Copy, Default)]
pub(crate) struct SurfaceRealizationStats {
    pub exact_requests: usize,
    pub cache_hits: usize,
    pub matrix_requests: usize,
}

#[async_trait]
pub(crate) trait SurfaceMovementPlanner: Send {
    fn kind(&self) -> SurfaceKind;
    fn realization_stats(&self) -> SurfaceRealizationStats {
        SurfaceRealizationStats::default()
    }
    fn optimistic_duration(&self, request: &SurfaceRequest<'_>) -> Option<u32>;
    async fn realize(
        &mut self,
        request: &SurfaceRequest<'_>,
    ) -> Result<Vec<SurfaceMovement>, StreetRoutingError>;
}

pub(crate) struct WalkingMovementPlanner {
    street: Arc<dyn StreetRouter>,
    cache: HashMap<
        (CorridorAnchorKey, CorridorAnchorKey, OffsetDateTime),
        Result<Arc<StreetPath>, StreetRoutingError>,
    >,
}

impl WalkingMovementPlanner {
    pub fn new(street: Arc<dyn StreetRouter>) -> Self {
        Self {
            street,
            cache: HashMap::new(),
        }
    }
}

#[async_trait]
impl SurfaceMovementPlanner for WalkingMovementPlanner {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Walking
    }
    fn optimistic_duration(&self, request: &SurfaceRequest<'_>) -> Option<u32> {
        // An optimistic pedestrian speed keeps this bound below ordinary routed walking.
        Some(
            (request.from.distance_to(request.to) / 2.0)
                .floor()
                .clamp(0.0, f64::from(u32::MAX)) as u32,
        )
    }
    async fn realize(
        &mut self,
        request: &SurfaceRequest<'_>,
    ) -> Result<Vec<SurfaceMovement>, StreetRoutingError> {
        // Time is part of the key because street routing receives a departure constraint.
        let key = (
            request.from.cache_key(),
            request.to.cache_key(),
            request.departure_at,
        );
        if !self.cache.contains_key(&key) {
            let local = normalize_to_taipei(request.departure_at);
            let result = self
                .street
                .route(RouteQuery {
                    travel_mode: StreetTravelMode::Walking,
                    origin: request.from.coordinate,
                    destination: request.to.coordinate,
                    constraints: RoutingConstraints {
                        time: Some(RoutingTime {
                            kind: RoutingTimeKind::DepartAt,
                            local_date_time: local.date().with_time(local.time()),
                        }),
                        max_distance: None,
                    },
                })
                .await
                .map(Arc::new);
            self.cache.insert(key.clone(), result);
        }
        self.cache[&key]
            .clone()
            .map(|path| vec![SurfaceMovement::Walking(path)])
    }
}
