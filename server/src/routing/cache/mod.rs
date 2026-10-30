#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    future::Future,
    hash::Hash,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::OnceCell;

use crate::street::{
    Coordinate, MatrixLimits, MatrixQuery, MatrixResult, ProviderFailureClass, RouteQuery,
    StreetPath, StreetRouter, StreetRoutingError,
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
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .and_modify(|_| {
                self.hits.fetch_add(1, Ordering::Relaxed);
            })
            .or_default()
            .clone();
        cell.get_or_init(init).await.clone()
    }

    pub(super) fn entries(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
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
    cache: RequestCache<MatrixKey, Result<MatrixResult, StreetRoutingError>>,
}

impl MatrixCache {
    pub(crate) fn new(provider: Arc<dyn StreetRouter>) -> Self {
        Self {
            provider,
            cache: RequestCache::default(),
        }
    }

    pub(crate) fn requests(&self) -> usize {
        self.cache.entries()
    }

    pub(crate) fn hits(&self) -> usize {
        self.cache.hits()
    }
}

#[async_trait::async_trait]
impl StreetRouter for MatrixCache {
    fn matrix_limits(&self) -> MatrixLimits {
        self.provider.matrix_limits()
    }

    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        let key = MatrixKey::new(&query)?;
        self.cache
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

type RouteEntry = Arc<OnceCell<Result<StreetPath, StreetRoutingError>>>;

pub(crate) struct RouteCache {
    provider: Arc<dyn StreetRouter>,
    entries: Mutex<HashMap<RouteKey, RouteEntry>>,
}

impl RouteCache {
    pub(crate) fn new(provider: Arc<dyn StreetRouter>) -> Self {
        Self {
            provider,
            entries: Mutex::new(HashMap::new()),
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
        let key = (
            query.travel_mode,
            coordinate_key(query.origin)?,
            coordinate_key(query.destination)?,
            query.constraints,
        );
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            match entries.get(&key) {
                Some(cell) => Some(cell.clone()),
                None if entries.len() < MAX_ROUTE_ENTRIES => {
                    Some(entries.entry(key).or_default().clone())
                }
                None => None,
            }
        };
        let Some(cell) = cell else {
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
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if entries
                .get(&key)
                .is_some_and(|entry| Arc::ptr_eq(entry, &cell))
            {
                entries.remove(&key);
            }
        }
        result
    }
}
