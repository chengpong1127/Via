use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;

use crate::health::DependencyHealth;

use super::{
    Coordinate, MatrixCell, MatrixQuery, MatrixResult, Metres, RouteQuery, Seconds, StreetPath,
    StreetRouter, StreetRoutingError, StreetTravelMode,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CoordinateKey {
    latitude: u64,
    longitude: u64,
}

impl From<Coordinate> for CoordinateKey {
    fn from(coordinate: Coordinate) -> Self {
        Self {
            latitude: coordinate.latitude().to_bits(),
            longitude: coordinate.longitude().to_bits(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct LegKey {
    travel_mode: StreetTravelMode,
    origin: CoordinateKey,
    destination: CoordinateKey,
}

impl LegKey {
    fn new(travel_mode: StreetTravelMode, origin: Coordinate, destination: Coordinate) -> Self {
        Self {
            travel_mode,
            origin: origin.into(),
            destination: destination.into(),
        }
    }
}

#[derive(Clone, Debug)]
enum MatrixFixture {
    Reachable { duration: Seconds, distance: Metres },
    Unreachable,
}

#[derive(Clone)]
pub struct FakeStreetRouter {
    healthy: bool,
    matrix: Arc<HashMap<LegKey, MatrixFixture>>,
    paths: Arc<HashMap<LegKey, StreetPath>>,
    matrix_failure: Option<StreetRoutingError>,
    route_failure: Option<StreetRoutingError>,
}

pub struct FakeStreetRouterBuilder {
    healthy: bool,
    matrix: HashMap<LegKey, MatrixFixture>,
    paths: HashMap<LegKey, StreetPath>,
    matrix_failure: Option<StreetRoutingError>,
    route_failure: Option<StreetRoutingError>,
}

impl FakeStreetRouter {
    pub fn builder() -> FakeStreetRouterBuilder {
        FakeStreetRouterBuilder {
            healthy: true,
            matrix: HashMap::new(),
            paths: HashMap::new(),
            matrix_failure: None,
            route_failure: None,
        }
    }
}

impl FakeStreetRouterBuilder {
    pub fn unhealthy(mut self) -> Self {
        self.healthy = false;
        self
    }

    pub fn matrix_failure(mut self, error: StreetRoutingError) -> Self {
        self.matrix_failure = Some(error);
        self
    }

    pub fn route_failure(mut self, error: StreetRoutingError) -> Self {
        self.route_failure = Some(error);
        self
    }

    pub fn matrix_leg(
        mut self,
        travel_mode: StreetTravelMode,
        origin: Coordinate,
        destination: Coordinate,
        duration: Seconds,
        distance: Metres,
    ) -> Self {
        self.matrix.insert(
            LegKey::new(travel_mode, origin, destination),
            MatrixFixture::Reachable { duration, distance },
        );
        self
    }

    pub fn unreachable_leg(
        mut self,
        travel_mode: StreetTravelMode,
        origin: Coordinate,
        destination: Coordinate,
    ) -> Self {
        self.matrix.insert(
            LegKey::new(travel_mode, origin, destination),
            MatrixFixture::Unreachable,
        );
        self
    }

    pub fn path(
        mut self,
        travel_mode: StreetTravelMode,
        origin: Coordinate,
        destination: Coordinate,
        path: StreetPath,
    ) -> Self {
        self.paths
            .insert(LegKey::new(travel_mode, origin, destination), path);
        self
    }

    pub fn build(self) -> FakeStreetRouter {
        FakeStreetRouter {
            healthy: self.healthy,
            matrix: Arc::new(self.matrix),
            paths: Arc::new(self.paths),
            matrix_failure: self.matrix_failure,
            route_failure: self.route_failure,
        }
    }
}

#[async_trait]
impl DependencyHealth for FakeStreetRouter {
    async fn healthy(&self) -> bool {
        self.healthy
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}

#[async_trait]
impl StreetRouter for FakeStreetRouter {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        query.validate()?;
        if let Some(error) = &self.matrix_failure {
            return Err(error.clone());
        }
        let cells = query
            .sources
            .iter()
            .map(|source| {
                query
                    .targets
                    .iter()
                    .map(|target| {
                        let key = LegKey::new(query.travel_mode, *source, *target);
                        match self.matrix.get(&key) {
                            Some(MatrixFixture::Reachable { duration, distance })
                                if query
                                    .constraints
                                    .max_distance
                                    .is_none_or(|maximum| distance.0 <= maximum.0) =>
                            {
                                MatrixCell::Reachable {
                                    duration: *duration,
                                    distance: *distance,
                                }
                            }
                            Some(MatrixFixture::Reachable { .. })
                            | Some(MatrixFixture::Unreachable) => MatrixCell::Unreachable,
                            None => self
                                .paths
                                .get(&key)
                                .filter(|path| {
                                    query
                                        .constraints
                                        .max_distance
                                        .is_none_or(|maximum| path.distance.0 <= maximum.0)
                                })
                                .map(|path| MatrixCell::Reachable {
                                    duration: path.duration,
                                    distance: path.distance,
                                })
                                .unwrap_or(MatrixCell::Unreachable),
                        }
                    })
                    .collect()
            })
            .collect();
        Ok(MatrixResult { cells })
    }

    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        if let Some(error) = &self.route_failure {
            return Err(error.clone());
        }
        let path = self
            .paths
            .get(&LegKey::new(
                query.travel_mode,
                query.origin,
                query.destination,
            ))
            .cloned()
            .ok_or(StreetRoutingError::Unreachable)?;
        if query
            .constraints
            .max_distance
            .is_some_and(|maximum| path.distance.0 > maximum.0)
        {
            return Err(StreetRoutingError::Unreachable);
        }
        Ok(path)
    }
}
