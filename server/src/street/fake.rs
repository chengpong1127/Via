use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{
    Coordinate, Correlation, DetailedPath, DetailedPathQuery, GraphRevision, MatrixCell,
    MatrixQuery, MatrixReachability, MatrixResult, Metres, ProviderCapabilities, ProviderCategory,
    ProviderError, ProviderStatus, Seconds, StreetRoutingProvider, StreetTravelMode,
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
pub struct FakeProvider {
    revision: Arc<RwLock<GraphRevision>>,
    matrix: Arc<HashMap<LegKey, MatrixFixture>>,
    paths: Arc<HashMap<LegKey, DetailedPath>>,
    status_failure: Option<ProviderError>,
    correlation_failure: Option<ProviderError>,
    matrix_failure: Option<ProviderError>,
    path_failure: Option<ProviderError>,
}

pub struct FakeProviderBuilder {
    revision: GraphRevision,
    matrix: HashMap<LegKey, MatrixFixture>,
    paths: HashMap<LegKey, DetailedPath>,
    status_failure: Option<ProviderError>,
    correlation_failure: Option<ProviderError>,
    matrix_failure: Option<ProviderError>,
    path_failure: Option<ProviderError>,
}

impl FakeProvider {
    pub fn builder(revision: GraphRevision) -> FakeProviderBuilder {
        FakeProviderBuilder {
            revision,
            matrix: HashMap::new(),
            paths: HashMap::new(),
            status_failure: None,
            correlation_failure: None,
            matrix_failure: None,
            path_failure: None,
        }
    }

    pub async fn publish_revision(&self, revision: GraphRevision) {
        *self.revision.write().await = revision;
    }

    async fn require_revision(
        &self,
        expected: &GraphRevision,
    ) -> Result<GraphRevision, ProviderError> {
        let actual = self.revision.read().await.clone();
        if &actual != expected {
            return Err(ProviderError::InconsistentGraphRevision {
                expected: expected.clone(),
                actual,
            });
        }
        Ok(actual)
    }
}

impl FakeProviderBuilder {
    pub fn status_failure(mut self, error: ProviderError) -> Self {
        self.status_failure = Some(error);
        self
    }

    pub fn correlation_failure(mut self, error: ProviderError) -> Self {
        self.correlation_failure = Some(error);
        self
    }

    pub fn matrix_failure(mut self, error: ProviderError) -> Self {
        self.matrix_failure = Some(error);
        self
    }

    pub fn path_failure(mut self, error: ProviderError) -> Self {
        self.path_failure = Some(error);
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
        path: DetailedPath,
    ) -> Self {
        self.paths
            .insert(LegKey::new(travel_mode, origin, destination), path);
        self
    }

    pub fn build(self) -> FakeProvider {
        FakeProvider {
            revision: Arc::new(RwLock::new(self.revision)),
            matrix: Arc::new(self.matrix),
            paths: Arc::new(self.paths),
            status_failure: self.status_failure,
            correlation_failure: self.correlation_failure,
            matrix_failure: self.matrix_failure,
            path_failure: self.path_failure,
        }
    }
}

#[async_trait]
impl StreetRoutingProvider for FakeProvider {
    async fn status(&self) -> Result<ProviderStatus, ProviderError> {
        if let Some(error) = &self.status_failure {
            return Err(error.clone());
        }
        Ok(ProviderStatus {
            category: ProviderCategory::Fake,
            healthy: true,
            graph_revision: self.revision.read().await.clone(),
            capabilities: ProviderCapabilities::REQUIRED,
            attribution: vec![],
        })
    }

    async fn correlate(
        &self,
        graph_revision: &GraphRevision,
        coordinates: &[Coordinate],
    ) -> Result<Vec<Correlation>, ProviderError> {
        self.require_revision(graph_revision).await?;
        if let Some(error) = &self.correlation_failure {
            return Err(error.clone());
        }
        Ok(coordinates
            .iter()
            .copied()
            .map(|coordinate| Correlation {
                input: coordinate,
                correlated: coordinate,
            })
            .collect())
    }

    async fn matrix(
        &self,
        graph_revision: &GraphRevision,
        query: &MatrixQuery,
    ) -> Result<MatrixResult, ProviderError> {
        let revision = self.require_revision(graph_revision).await?;
        if let Some(error) = &self.matrix_failure {
            return Err(error.clone());
        }
        let cells = query
            .sources
            .iter()
            .enumerate()
            .map(|(source_index, source)| {
                query
                    .targets
                    .iter()
                    .enumerate()
                    .map(|(target_index, target)| {
                        let key = LegKey::new(query.travel_mode, *source, *target);
                        let fixture = self.matrix.get(&key);
                        let reachability = match fixture {
                            Some(MatrixFixture::Reachable { duration, distance })
                                if query
                                    .constraints
                                    .max_distance
                                    .is_none_or(|maximum| distance.0 <= maximum.0) =>
                            {
                                MatrixReachability::Reachable {
                                    duration: *duration,
                                    distance: *distance,
                                }
                            }
                            Some(MatrixFixture::Reachable { .. })
                            | Some(MatrixFixture::Unreachable) => MatrixReachability::Unreachable,
                            None => self
                                .paths
                                .get(&key)
                                .filter(|path| {
                                    query
                                        .constraints
                                        .max_distance
                                        .is_none_or(|maximum| path.distance.0 <= maximum.0)
                                })
                                .map(|path| MatrixReachability::Reachable {
                                    duration: path.duration,
                                    distance: path.distance,
                                })
                                .unwrap_or(MatrixReachability::Unreachable),
                        };
                        MatrixCell {
                            source_index,
                            target_index,
                            source_correlation: Some(*source),
                            target_correlation: Some(*target),
                            reachability,
                        }
                    })
                    .collect()
            })
            .collect();
        Ok(MatrixResult {
            cells,
            graph_revision: revision,
        })
    }

    async fn detailed_path(
        &self,
        graph_revision: &GraphRevision,
        query: &DetailedPathQuery,
    ) -> Result<DetailedPath, ProviderError> {
        self.require_revision(graph_revision).await?;
        if let Some(error) = &self.path_failure {
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
            .ok_or(ProviderError::UnreachableEndpoints)?;
        if query
            .constraints
            .max_distance
            .is_some_and(|maximum| path.distance.0 > maximum.0)
        {
            return Err(ProviderError::UnreachableEndpoints);
        }
        Ok(path)
    }
}
