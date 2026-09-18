use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures_util::future::try_join_all;
use moka::future::Cache;

use crate::domain::{
    ids::TransitServiceId,
    realtime::{FreshnessReason, FreshnessState},
    transit_timing::DepartureError,
};

pub use crate::domain::realtime::{NormalizedRealtimeValue, RealtimeValue, RealtimeValueKind};

pub const DEFAULT_REALTIME_CACHE_TTL: Duration = Duration::from_secs(45);

/// Metrics shared by status collection and mode-owned departure sources.
/// The cache itself is intentionally limited to service-condition lookups;
/// bus departure freshness and batching live in `BusDepartureSource`.
#[derive(Clone, Default)]
pub struct RealtimeMetrics {
    cache_hits: Arc<AtomicU64>,
    cache_misses: Arc<AtomicU64>,
    upstream_requests: Arc<AtomicU64>,
    upstream_failures: Arc<AtomicU64>,
    rate_limited_requests: Arc<AtomicU64>,
    logical_departure_needs: Arc<AtomicU64>,
    deduplicated_departure_needs: Arc<AtomicU64>,
    departure_batches: Arc<AtomicU64>,
    departure_upstream_requests: Arc<AtomicU64>,
    fresh_values: Arc<AtomicU64>,
    degraded_values: Arc<AtomicU64>,
    unavailable_values: Arc<AtomicU64>,
    not_applicable_values: Arc<AtomicU64>,
}

impl RealtimeMetrics {
    fn cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    fn upstream_request(&self) {
        self.upstream_requests.fetch_add(1, Ordering::Relaxed);
    }

    fn upstream_result(&self, error: Option<&RealtimeError>) {
        if let Some(error) = error {
            self.upstream_failures.fetch_add(1, Ordering::Relaxed);
            if matches!(error, RealtimeError::RateLimited) {
                self.rate_limited_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn record_departure_batch(
        &self,
        logical_needs: usize,
        unique_needs: usize,
        batch_count: usize,
    ) {
        self.logical_departure_needs
            .fetch_add(logical_needs as u64, Ordering::Relaxed);
        self.deduplicated_departure_needs
            .fetch_add(unique_needs as u64, Ordering::Relaxed);
        self.departure_batches
            .fetch_add(batch_count as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_departure_upstream_request(&self) {
        self.departure_upstream_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_departure_upstream_result(&self, error: Option<&DepartureError>) {
        if let Some(error) = error {
            self.upstream_failures.fetch_add(1, Ordering::Relaxed);
            if matches!(error, DepartureError::RateLimited) {
                self.rate_limited_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn bus_timing_trace(&self) -> String {
        format!(
            "BUS TIMING\n  logical needs={}\n  unique needs={}\n  upstream batches={}\n  upstream calls={}",
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.deduplicated_departure_needs.load(Ordering::Relaxed),
            self.departure_batches.load(Ordering::Relaxed),
            self.departure_upstream_requests.load(Ordering::Relaxed),
        )
    }

    fn observe_values(&self, values: &[NormalizedRealtimeValue]) {
        for value in values {
            let counter = match value.freshness.state {
                FreshnessState::Fresh => &self.fresh_values,
                FreshnessState::Degraded => &self.degraded_values,
                FreshnessState::Unavailable => &self.unavailable_values,
                FreshnessState::NotApplicable => &self.not_applicable_values,
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn prometheus(&self) -> String {
        format!(
            "# TYPE via_realtime_cache_hits_total counter\nvia_realtime_cache_hits_total {}\n\
# TYPE via_realtime_cache_misses_total counter\nvia_realtime_cache_misses_total {}\n\
# TYPE via_realtime_upstream_requests_total counter\nvia_realtime_upstream_requests_total {}\n\
# TYPE via_realtime_upstream_failures_total counter\nvia_realtime_upstream_failures_total {}\n\
# TYPE via_realtime_rate_limited_requests_total counter\nvia_realtime_rate_limited_requests_total {}\n\
# TYPE via_realtime_logical_departure_needs_total counter\nvia_realtime_logical_departure_needs_total {}\n\
# TYPE via_realtime_deduplicated_departure_needs_total counter\nvia_realtime_deduplicated_departure_needs_total {}\n\
# TYPE via_realtime_departure_batches_total counter\nvia_realtime_departure_batches_total {}\n\
# TYPE via_realtime_departure_upstream_requests_total counter\nvia_realtime_departure_upstream_requests_total {}\n\
# TYPE via_realtime_fresh_values_total counter\nvia_realtime_fresh_values_total {}\n\
# TYPE via_realtime_degraded_values_total counter\nvia_realtime_degraded_values_total {}\n\
# TYPE via_realtime_unavailable_values_total counter\nvia_realtime_unavailable_values_total {}\n\
# TYPE via_realtime_not_applicable_values_total counter\nvia_realtime_not_applicable_values_total {}\n",
            self.cache_hits.load(Ordering::Relaxed),
            self.cache_misses.load(Ordering::Relaxed),
            self.upstream_requests.load(Ordering::Relaxed),
            self.upstream_failures.load(Ordering::Relaxed),
            self.rate_limited_requests.load(Ordering::Relaxed),
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.deduplicated_departure_needs.load(Ordering::Relaxed),
            self.departure_batches.load(Ordering::Relaxed),
            self.departure_upstream_requests.load(Ordering::Relaxed),
            self.fresh_values.load(Ordering::Relaxed),
            self.degraded_values.load(Ordering::Relaxed),
            self.unavailable_values.load(Ordering::Relaxed),
            self.not_applicable_values.load(Ordering::Relaxed),
        )
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RealtimeError {
    #[error("realtime provider timed out")]
    TimedOut,
    #[error("realtime provider rate limited the request")]
    RateLimited,
    #[error("realtime provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("realtime provider returned a rejected payload")]
    RejectedPayload,
    #[error("realtime provider returned an invalid response")]
    InvalidResponse,
    #[error("realtime provider configuration is invalid")]
    InvalidConfiguration,
}

impl RealtimeError {
    pub fn freshness_reason(&self) -> FreshnessReason {
        match self {
            Self::TimedOut => FreshnessReason::TimedOut,
            Self::RateLimited => FreshnessReason::RateLimited,
            Self::TemporarilyUnavailable => FreshnessReason::SourceError,
            Self::RejectedPayload | Self::InvalidResponse => FreshnessReason::RejectedPayload,
            Self::InvalidConfiguration => FreshnessReason::SourceError,
        }
    }

    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::TimedOut | Self::RateLimited | Self::TemporarilyUnavailable
        )
    }
}

#[async_trait]
pub trait RealtimeProvider: Send + Sync {
    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError>;
}

#[derive(Clone)]
pub struct CachedRealtimeProvider {
    inner: Arc<dyn RealtimeProvider>,
    condition_cache: Cache<TransitServiceId, Arc<Vec<NormalizedRealtimeValue>>>,
    ttl: Duration,
    metrics: RealtimeMetrics,
}

impl CachedRealtimeProvider {
    pub fn new(inner: Arc<dyn RealtimeProvider>) -> Self {
        Self::with_ttl(inner, DEFAULT_REALTIME_CACHE_TTL)
    }

    pub fn with_ttl(inner: Arc<dyn RealtimeProvider>, ttl: Duration) -> Self {
        let ttl = ttl.max(Duration::from_millis(1));
        Self {
            inner,
            condition_cache: Cache::builder().time_to_live(ttl).build(),
            ttl,
            metrics: RealtimeMetrics::default(),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn metrics(&self) -> RealtimeMetrics {
        self.metrics.clone()
    }

    pub async fn clear(&self) {
        self.condition_cache.invalidate_all();
    }

    async fn service_condition(
        &self,
        service_id: TransitServiceId,
    ) -> Result<Arc<Vec<NormalizedRealtimeValue>>, RealtimeError> {
        let inner = self.inner.clone();
        let key = service_id.clone();
        if self.condition_cache.get(&key).await.is_some() {
            self.metrics.cache_hit();
        } else {
            self.metrics.cache_miss();
        }
        let metrics = self.metrics.clone();
        self.condition_cache
            .try_get_with(key, async move {
                metrics.upstream_request();
                let result = inner
                    .service_conditions(std::slice::from_ref(&service_id))
                    .await
                    .map(Arc::new);
                if let Ok(values) = &result {
                    metrics.observe_values(values);
                }
                metrics.upstream_result(result.as_ref().err());
                result
            })
            .await
            .map_err(|error| (*error).clone())
    }
}

#[async_trait]
impl RealtimeProvider for CachedRealtimeProvider {
    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        let mut unique = HashSet::new();
        let results = try_join_all(
            service_ids
                .iter()
                .filter(|service_id| unique.insert((*service_id).clone()))
                .cloned()
                .map(|service_id| self.service_condition(service_id)),
        )
        .await?;
        Ok(results
            .into_iter()
            .flat_map(|values| values.as_ref().clone())
            .collect())
    }
}

#[derive(Default)]
pub struct NullRealtimeProvider;

#[async_trait]
impl RealtimeProvider for NullRealtimeProvider {
    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        Ok(Vec::new())
    }
}
