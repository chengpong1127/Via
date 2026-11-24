#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    future::Future,
    hash::Hash,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::OnceCell;

use crate::street::{
    Coordinate, MatrixCell, MatrixLimits, MatrixQuery, MatrixResult, ProviderFailureClass,
    RouteQuery, StreetPath, StreetRouter, StreetRoutingError,
};

const MAX_ROUTE_ENTRIES: usize = 4096;

pub(super) struct RequestCache<K, V> {
    entries: Mutex<HashMap<K, Arc<OnceCell<V>>>>,
    hits: AtomicUsize,
}

impl<K, V> Default for RequestCache<K, V> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            hits: AtomicUsize::new(0),
        }
    }
}

impl<K: Eq + Hash, V: Clone> RequestCache<K, V> {
    pub(super) async fn get_or_init<F, Fut>(&self, key: K, init: F) -> V
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V>,
    {
        let cell = self
            .lock()
            .entry(key)
            .and_modify(|_| {
                self.hits.fetch_add(1, Ordering::Relaxed);
            })
            .or_default()
            .clone();
        cell.get_or_init(init).await.clone()
    }

    fn cell_within(&self, key: K, capacity: usize) -> Option<Arc<OnceCell<V>>> {
        let mut entries = self.lock();
        match entries.get(&key) {
            Some(cell) => Some(cell.clone()),
            None if entries.len() < capacity => Some(entries.entry(key).or_default().clone()),
            None => None,
        }
    }

    fn forget(&self, key: &K, cell: &Arc<OnceCell<V>>) {
        let mut entries = self.lock();
        if entries
            .get(key)
            .is_some_and(|entry| Arc::ptr_eq(entry, cell))
        {
            entries.remove(key);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<K, Arc<OnceCell<V>>>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn entries(&self) -> usize {
        self.lock().len()
    }

    pub(super) fn hits(&self) -> usize {
        self.hits.load(Ordering::Relaxed)
    }
}

fn coordinate_key(point: Coordinate) -> Result<(u64, u64), StreetRoutingError> {
    Coordinate::try_new(point.latitude(), point.longitude())?;
    Ok((point.latitude().to_bits(), point.longitude().to_bits()))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MatrixKey {
    query: (
        crate::street::MatrixOrientation,
        crate::street::StreetTravelMode,
        crate::street::RoutingConstraints,
    ),
    sources: Vec<(u64, u64)>,
    targets: Vec<(u64, u64)>,
}

impl MatrixKey {
    fn new(query: &MatrixQuery) -> Result<Self, StreetRoutingError> {
        query.validate()?;
        let keys = |points: &[Coordinate]| -> Result<Vec<_>, StreetRoutingError> {
            points.iter().copied().map(coordinate_key).collect()
        };
        Ok(Self {
            query: (query.orientation, query.travel_mode, query.constraints),
            sources: keys(&query.sources)?,
            targets: keys(&query.targets)?,
        })
    }
}

pub(crate) struct MatrixCache {
    provider: Arc<dyn StreetRouter>,
    matrices: RequestCache<MatrixKey, Result<MatrixResult, StreetRoutingError>>,
    route_cells: RequestCache<RouteKey, Result<MatrixCell, StreetRoutingError>>,
}

impl MatrixCache {
    pub(crate) fn new(provider: Arc<dyn StreetRouter>) -> Self {
        Self {
            provider,
            matrices: RequestCache::default(),
            route_cells: RequestCache::default(),
        }
    }

    pub(crate) fn requests(&self) -> usize {
        self.matrices.entries() + self.route_cells.entries()
    }

    pub(crate) fn hits(&self) -> usize {
        self.matrices.hits() + self.route_cells.hits()
    }

    pub(crate) async fn route_cell(
        &self,
        query: RouteQuery,
    ) -> Result<MatrixCell, StreetRoutingError> {
        let key = route_key(&query)?;
        self.route_cells
            .get_or_init(key, || async {
                match self.provider.route(query).await {
                    Ok(path) => Ok(MatrixCell::Reachable {
                        duration: path.duration,
                        distance: path.distance,
                    }),
                    Err(StreetRoutingError::Unreachable) => Ok(MatrixCell::Unreachable),
                    Err(error) => Err(error),
                }
            })
            .await
    }
}

#[async_trait::async_trait]
impl StreetRouter for MatrixCache {
    fn matrix_limits(&self) -> MatrixLimits {
        self.provider.matrix_limits()
    }

    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        let key = MatrixKey::new(&query)?;
        self.matrices
            .get_or_init(key, || self.provider.matrix(query))
            .await
    }

    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        self.provider.route(query).await
    }
}

type RouteKey = (
    crate::street::StreetTravelMode,
    (u64, u64),
    (u64, u64),
    crate::street::RoutingConstraints,
);

fn route_key(query: &RouteQuery) -> Result<RouteKey, StreetRoutingError> {
    Ok((
        query.travel_mode,
        coordinate_key(query.origin)?,
        coordinate_key(query.destination)?,
        query.constraints,
    ))
}

pub(crate) struct RouteCache {
    provider: Arc<dyn StreetRouter>,
    paths: RequestCache<RouteKey, Result<StreetPath, StreetRoutingError>>,
}

impl RouteCache {
    pub(crate) fn new(provider: Arc<dyn StreetRouter>) -> Self {
        Self {
            provider,
            paths: RequestCache::default(),
        }
    }
}

#[async_trait::async_trait]
impl StreetRouter for RouteCache {
    fn matrix_limits(&self) -> MatrixLimits {
        self.provider.matrix_limits()
    }

    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        self.provider.matrix(query).await
    }

    #[tracing::instrument(target = "via_profile", name = "Route Cache", skip_all)]
    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        if query.constraints.time.is_some() {
            return self.provider.route(query).await;
        }
        let key = route_key(&query)?;
        let Some(cell) = self.paths.cell_within(key, MAX_ROUTE_ENTRIES) else {
            return self.provider.route(query).await;
        };
        let result = cell
            .get_or_init(|| self.provider.route(query))
            .await
            .clone();
        if result
            .as_ref()
            .is_err_and(|error| error.failure_class() != ProviderFailureClass::Unreachable)
        {
            self.paths.forget(&key, &cell);
        }
        result
    }
}
