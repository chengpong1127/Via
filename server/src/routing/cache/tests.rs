use super::MatrixCache;
use crate::street::*;
use futures_util::{StreamExt, stream};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

struct Controlled {
    started: Semaphore,
    release: Semaphore,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    result: Result<MatrixResult, StreetRoutingError>,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Controlled {
    fn new(result: Result<MatrixResult, StreetRoutingError>) -> Arc<Self> {
        Arc::new(Self {
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            result,
        })
    }
    async fn wait_started(&self, count: u32) {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.started.acquire_many(count),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
    }
}
#[async_trait::async_trait]
impl StreetRouter for Controlled {
    async fn matrix(&self, _: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        self.started.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        self.result.clone()
    }
    async fn route(&self, _: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        Err(StreetRoutingError::Unreachable)
    }
}
fn query() -> MatrixQuery {
    MatrixQuery {
        orientation: MatrixOrientation::OneToMany,
        travel_mode: StreetTravelMode::Walking,
        sources: vec![Coordinate::try_new(25.0, 121.0).unwrap()],
        targets: vec![Coordinate::try_new(25.1, 121.1).unwrap()],
        constraints: RoutingConstraints::default(),
    }
}
fn success() -> Result<MatrixResult, StreetRoutingError> {
    Ok(MatrixResult {
        cells: vec![vec![MatrixCell::Unreachable]],
    })
}

#[tokio::test]
async fn identical_misses_and_every_failure_are_shared_and_reused() {
    for result in [
        success(),
        Err(StreetRoutingError::Unreachable),
        Err(StreetRoutingError::Timeout),
        Err(StreetRoutingError::RateLimited),
        Err(StreetRoutingError::TemporarilyUnavailable),
        Err(StreetRoutingError::MalformedResponse),
        Err(StreetRoutingError::InvalidRequest {
            reason: "provider".into(),
        }),
        Err(StreetRoutingError::ResponseTooLarge { limit_bytes: 10 }),
    ] {
        let provider = Controlled::new(result.clone());
        let resolver = Arc::new(MatrixCache::new(provider.clone()));
        let joined = tokio::spawn({
            let resolver = resolver.clone();
            async move {
                stream::iter(0..10)
                    .map(|_| resolver.matrix(query()))
                    .buffer_unordered(9)
                    .collect::<Vec<_>>()
                    .await
            }
        });
        provider.wait_started(1).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        provider.release.add_permits(1);
        for actual in joined.await.unwrap() {
            assert_eq!(actual, result);
        }
        assert_eq!(resolver.matrix(query()).await, result);
        assert_eq!((resolver.requests(), resolver.hits()), (1, 10));
    }
}

#[tokio::test]
async fn exact_keys_allow_distinct_queries_to_overlap_and_requests_are_isolated() {
    use time::macros::datetime;
    let base = query();
    let mut queries = vec![base.clone()];
    let mut q = base.clone();
    q.travel_mode = StreetTravelMode::Bicycle;
    queries.push(q);
    let mut q = base.clone();
    std::mem::swap(&mut q.sources, &mut q.targets);
    queries.push(q);
    let mut q = base.clone();
    q.orientation = MatrixOrientation::ManyToOne;
    queries.push(q);
    let mut q = base.clone();
    q.sources[0] = Coordinate::try_new(25.00000000001, 121.0).unwrap();
    queries.push(q);
    let mut q = base.clone();
    q.targets[0] = Coordinate::try_new(25.10000000001, 121.1).unwrap();
    queries.push(q);
    let mut q = base.clone();
    q.constraints.max_distance = Some(Metres(0));
    queries.push(q);
    for nanos in [0, 1] {
        let mut q = base.clone();
        let instant = datetime!(2026-09-21 10:00 UTC) + time::Duration::nanoseconds(nanos);
        q.constraints.time = Some(RoutingTime {
            local_date_time: instant.date().with_time(instant.time()),
        });
        queries.push(q);
    }
    let provider = Controlled::new(success());
    let resolver = Arc::new(MatrixCache::new(provider.clone()));
    let work = tokio::spawn({
        let resolver = resolver.clone();
        async move {
            stream::iter(queries)
                .map(|q| resolver.matrix(q))
                .buffer_unordered(10)
                .collect::<Vec<_>>()
                .await
        }
    });
    provider.wait_started(9).await;
    assert_eq!(provider.peak.load(Ordering::SeqCst), 9);
    provider.release.add_permits(9);
    assert!(work.await.unwrap().iter().all(Result::is_ok));
    let second = MatrixCache::new(provider.clone());
    provider.release.add_permits(1);
    second.matrix(base).await.unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn canceled_initializer_releases_waiters_and_canceled_waiter_does_not_cancel_initializer() {
    let provider = Controlled::new(success());
    let resolver = Arc::new(MatrixCache::new(provider.clone()));
    let launch = || {
        let resolver = resolver.clone();
        tokio::spawn(async move { resolver.matrix(query()).await })
    };
    let leader = launch();
    provider.wait_started(1).await;
    let waiter = launch();
    tokio::task::yield_now().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let survivor = launch();
    tokio::task::yield_now().await;
    leader.abort();
    assert!(leader.await.unwrap_err().is_cancelled());
    provider.wait_started(1).await;
    provider.release.add_permits(1);
    assert_eq!(survivor.await.unwrap(), success());
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(resolver.matrix(query()).await, success());
}

#[tokio::test(start_paused = true)]
async fn deadline_drop_cancels_io_and_leaves_the_cell_retryable() {
    let provider = Controlled::new(success());
    let resolver = MatrixCache::new(provider.clone());
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(1), resolver.matrix(query())).await;
    assert!(result.is_err());
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    provider.release.add_permits(1);
    assert_eq!(resolver.matrix(query()).await, success());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn matrix_order_and_subsecond_constraints_never_alias_and_invalid_coordinates_are_rejected() {
    let provider = Controlled::new(success());
    provider.release.add_permits(4);
    let resolver = MatrixCache::new(provider.clone());
    let mut base = query();
    base.orientation = MatrixOrientation::ManyToMany;
    base.sources.push(Coordinate::try_new(25.2, 121.2).unwrap());
    base.targets.push(Coordinate::try_new(25.3, 121.3).unwrap());
    for (sources, targets) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut query = base.clone();
        if sources {
            query.sources.reverse();
        }
        if targets {
            query.targets.reverse();
        }
        resolver.matrix(query).await.unwrap();
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let mut invalid = base;
    invalid.sources[0] =
        serde_json::from_value(serde_json::json!({"latitude": 91.0, "longitude": 121.0})).unwrap();
    assert!(matches!(
        resolver.matrix(invalid).await,
        Err(StreetRoutingError::InvalidRequest { .. })
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
}

mod route {
    use super::super::RouteCache;
    use crate::street::*;
    use crate::street::{Metres, Seconds};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
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
