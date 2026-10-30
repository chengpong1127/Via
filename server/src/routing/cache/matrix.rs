use super::request_cache::RequestCache;
use crate::street::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MatrixKey {
    orientation: MatrixOrientation,
    mode: StreetTravelMode,
    sources: Vec<(u64, u64)>,
    targets: Vec<(u64, u64)>,
    constraints: RoutingConstraints,
}

impl MatrixKey {
    fn new(query: &MatrixQuery) -> Result<Self, StreetRoutingError> {
        query.validate()?;
        let MatrixQuery {
            orientation,
            travel_mode,
            sources,
            targets,
            constraints,
        } = query;
        let coordinates = |points: &[Coordinate]| {
            points
                .iter()
                .map(|point| {
                    Coordinate::try_new(point.latitude(), point.longitude())?;
                    Ok((point.latitude().to_bits(), point.longitude().to_bits()))
                })
                .collect::<Result<Vec<_>, StreetRoutingError>>()
        };
        Ok(Self {
            orientation: *orientation,
            mode: *travel_mode,
            sources: coordinates(sources)?,
            targets: coordinates(targets)?,
            constraints: *constraints,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatrixResolverStats {
    pub logical_requests: usize,
    pub physical_requests: usize,
    pub completed_hits: usize,
    pub inflight_hits: usize,
    pub misses: usize,
    pub provider_failures: usize,
    pub provider_timeouts: usize,
    pub max_inflight: usize,
    pub canceled_requests: usize,
}

pub(crate) struct SharedMatrixResolver {
    provider: Arc<dyn StreetRouter>,
    cache: RequestCache<MatrixKey, Result<MatrixResult, StreetRoutingError>>,
    logical: AtomicUsize,
    failures: AtomicUsize,
    timeouts: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    canceled: AtomicUsize,
}

struct InFlight<'a> {
    resolver: &'a SharedMatrixResolver,
    completed: bool,
}
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.resolver.active.fetch_sub(1, Ordering::Relaxed);
        if !self.completed {
            self.resolver.canceled.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl SharedMatrixResolver {
    pub fn new(provider: Arc<dyn StreetRouter>) -> Self {
        Self {
            provider,
            cache: RequestCache::default(),
            logical: AtomicUsize::new(0),
            failures: AtomicUsize::new(0),
            timeouts: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            canceled: AtomicUsize::new(0),
        }
    }
    pub fn stats(&self) -> MatrixResolverStats {
        let cache = self.cache.stats();
        MatrixResolverStats {
            logical_requests: self.logical.load(Ordering::Relaxed),
            physical_requests: cache.initializations,
            completed_hits: cache.ready_hits,
            inflight_hits: cache.inflight_hits,
            misses: cache.misses,
            provider_failures: self.failures.load(Ordering::Relaxed),
            provider_timeouts: self.timeouts.load(Ordering::Relaxed),
            max_inflight: self.peak.load(Ordering::Relaxed),
            canceled_requests: self.canceled.load(Ordering::Relaxed),
        }
    }
}

#[async_trait::async_trait]
impl StreetRouter for SharedMatrixResolver {
    fn matrix_limits(&self) -> MatrixLimits {
        self.provider.matrix_limits()
    }
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        self.logical.fetch_add(1, Ordering::Relaxed);
        let key = MatrixKey::new(&query)?;
        self.cache
            .get_or_init(key, || async {
                let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
                self.peak.fetch_max(active, Ordering::Relaxed);
                let mut flight = InFlight {
                    resolver: self,
                    completed: false,
                };
                let result = self.provider.matrix(query).await;
                flight.completed = true;
                if let Err(error) = &result {
                    self.failures.fetch_add(1, Ordering::Relaxed);
                    if *error == StreetRoutingError::Timeout {
                        self.timeouts.fetch_add(1, Ordering::Relaxed);
                    }
                }
                result
            })
            .await
    }
    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        self.provider.route(query).await
    }
}
