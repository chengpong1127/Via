use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::OnceCell;

use crate::street::{
    Coordinate, MatrixLimits, MatrixQuery, MatrixResult, ProviderFailureClass, RouteQuery,
    RoutingConstraints, StreetPath, StreetRouter, StreetRoutingError, StreetTravelMode,
};

const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct RouteKey {
    mode: StreetTravelMode,
    origin: (u64, u64),
    destination: (u64, u64),
    constraints: RoutingConstraints,
}

impl RouteKey {
    fn new(query: &RouteQuery) -> Result<Self, StreetRoutingError> {
        let coordinate = |point: Coordinate| {
            Coordinate::try_new(point.latitude(), point.longitude())?;
            Ok((point.latitude().to_bits(), point.longitude().to_bits()))
        };
        Ok(Self {
            mode: query.travel_mode,
            origin: coordinate(query.origin)?,
            destination: coordinate(query.destination)?,
            constraints: query.constraints,
        })
    }
}

type Entry = OnceCell<Result<StreetPath, StreetRoutingError>>;

pub(crate) struct RouteCache {
    provider: Arc<dyn StreetRouter>,
    entries: Mutex<HashMap<RouteKey, Arc<Entry>>>,
    logical: AtomicUsize,
    ready: AtomicUsize,
    inflight: AtomicUsize,
    misses: AtomicUsize,
}

impl RouteCache {
    pub(crate) fn new(provider: Arc<dyn StreetRouter>) -> Self {
        Self {
            provider,
            entries: Mutex::new(HashMap::new()),
            logical: AtomicUsize::new(0),
            ready: AtomicUsize::new(0),
            inflight: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }
    #[cfg(test)]
    pub(crate) fn stats(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.logical.load(Ordering::Relaxed),
            self.ready.load(Ordering::Relaxed),
            self.inflight.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.entries.lock().unwrap_or_else(|e| e.into_inner()).len(),
        )
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

    #[tracing::instrument(target = "via_profile", name = "Route Cache", skip_all, fields(result = tracing::field::Empty))]
    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        if query.constraints.time.is_some() {
            tracing::Span::current().record("result", "bypass");
            return self.provider.route(query).await;
        }
        self.logical.fetch_add(1, Ordering::Relaxed);
        let key = RouteKey::new(&query)?;
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cell) = entries.get(&key) {
                if cell.initialized() {
                    tracing::Span::current().record("result", "ready");
                    self.ready.fetch_add(1, Ordering::Relaxed);
                } else {
                    tracing::Span::current().record("result", "inflight");
                    self.inflight.fetch_add(1, Ordering::Relaxed);
                }
                Some(cell.clone())
            } else if entries.len() < MAX_ENTRIES {
                tracing::Span::current().record("result", "miss");
                self.misses.fetch_add(1, Ordering::Relaxed);
                let cell = Arc::new(OnceCell::new());
                entries.insert(key, cell.clone());
                Some(cell)
            } else {
                tracing::Span::current().record("result", "capacity");
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::street::{Metres, Seconds};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Provider {
        calls: AtomicUsize,
        error: Mutex<Option<StreetRoutingError>>,
    }

    #[async_trait::async_trait]
    impl StreetRouter for Provider {
        async fn matrix(&self, _: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
            unreachable!()
        }
        async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
            assert!(query.constraints.time.is_none());
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Some(error) = self.error.lock().unwrap().take() {
                return Err(error);
            }
            Ok(StreetPath {
                duration: Seconds(180),
                distance: Metres(100),
                geometry: vec![query.origin, query.destination],
                maneuvers: vec![],
                attribution: vec![],
            })
        }
    }

    fn query() -> RouteQuery {
        RouteQuery {
            travel_mode: StreetTravelMode::Walking,
            origin: Coordinate::try_new(25.0, 121.5).unwrap(),
            destination: Coordinate::try_new(25.01, 121.51).unwrap(),
            constraints: RoutingConstraints::default(),
        }
    }

    #[tokio::test]
    async fn concurrent_routes_share_results_and_keys_preserve_mode_direction_and_constraints() {
        let provider = Arc::new(Provider {
            calls: AtomicUsize::new(0),
            error: Mutex::new(None),
        });
        let cache = RouteCache::new(provider.clone());
        let q = query();
        let results = futures_util::future::join_all((0..20).map(|_| cache.route(q.clone()))).await;
        assert!(
            results
                .iter()
                .all(|result| result.as_ref().unwrap().duration == Seconds(180))
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        cache.route(q.clone()).await.unwrap();
        let mut other = q.clone();
        other.travel_mode = StreetTravelMode::Bicycle;
        cache.route(other).await.unwrap();
        let mut other = q.clone();
        std::mem::swap(&mut other.origin, &mut other.destination);
        cache.route(other).await.unwrap();
        let mut other = q;
        other.constraints.max_distance = Some(Metres(200));
        cache.route(other).await.unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn failures_retry_except_unreachable_and_new_requests_are_isolated() {
        for error in [
            StreetRoutingError::Timeout,
            StreetRoutingError::RateLimited,
            StreetRoutingError::TemporarilyUnavailable,
            StreetRoutingError::MalformedResponse,
            StreetRoutingError::Unreachable,
        ] {
            let provider = Arc::new(Provider {
                calls: AtomicUsize::new(0),
                error: Mutex::new(Some(error.clone())),
            });
            let cache = RouteCache::new(provider.clone());
            assert_eq!(cache.route(query()).await.unwrap_err(), error);
            let result = cache.route(query()).await;
            assert_eq!(result.is_err(), error == StreetRoutingError::Unreachable);
            let fresh = RouteCache::new(provider.clone());
            assert!(fresh.route(query()).await.is_ok());
        }
    }

    #[tokio::test]
    async fn canceled_initializer_releases_waiters_and_future_requests() {
        struct CancelProvider {
            calls: AtomicUsize,
            started: tokio::sync::Notify,
        }

        #[async_trait::async_trait]
        impl StreetRouter for CancelProvider {
            async fn matrix(&self, _: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
                unreachable!()
            }

            async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    self.started.notify_one();
                    std::future::pending::<()>().await;
                }
                Ok(StreetPath {
                    duration: Seconds(180),
                    distance: Metres(100),
                    geometry: vec![query.origin, query.destination],
                    maneuvers: vec![],
                    attribution: vec![],
                })
            }
        }

        let provider = Arc::new(CancelProvider {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
        });
        let cache = Arc::new(RouteCache::new(provider.clone()));
        let task = tokio::spawn({
            let cache = cache.clone();
            async move { cache.route(query()).await }
        });
        provider.started.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), cache.route(query()))
                .await
                .unwrap()
                .is_ok()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
}
