//! Provider-neutral service-condition collection and caching.

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
    realtime::{FreshnessReason, FreshnessState, NormalizedRealtimeValue},
};

pub mod tdx;

pub use crate::domain::realtime::{RealtimeValue, RealtimeValueKind};
pub use tdx::TdxServiceConditionProvider;

pub const DEFAULT_SERVICE_CONDITION_CACHE_TTL: Duration = Duration::from_secs(45);

#[derive(Clone, Default)]
pub struct ServiceConditionMetrics {
    cache_hits: Arc<AtomicU64>,
    cache_misses: Arc<AtomicU64>,
    upstream_requests: Arc<AtomicU64>,
    upstream_failures: Arc<AtomicU64>,
    rate_limited_requests: Arc<AtomicU64>,
    fresh_values: Arc<AtomicU64>,
    degraded_values: Arc<AtomicU64>,
    unavailable_values: Arc<AtomicU64>,
    not_applicable_values: Arc<AtomicU64>,
}

impl ServiceConditionMetrics {
    fn cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    fn upstream_request(&self) {
        self.upstream_requests.fetch_add(1, Ordering::Relaxed);
    }

    fn upstream_result(&self, error: Option<&ServiceConditionError>) {
        if let Some(error) = error {
            self.upstream_failures.fetch_add(1, Ordering::Relaxed);
            if matches!(error, ServiceConditionError::RateLimited) {
                self.rate_limited_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
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
            "# TYPE via_service_condition_cache_hits_total counter\nvia_service_condition_cache_hits_total {}\n\
# TYPE via_service_condition_cache_misses_total counter\nvia_service_condition_cache_misses_total {}\n\
# TYPE via_service_condition_upstream_requests_total counter\nvia_service_condition_upstream_requests_total {}\n\
# TYPE via_service_condition_upstream_failures_total counter\nvia_service_condition_upstream_failures_total {}\n\
# TYPE via_service_condition_rate_limited_requests_total counter\nvia_service_condition_rate_limited_requests_total {}\n\
# TYPE via_service_condition_fresh_values_total counter\nvia_service_condition_fresh_values_total {}\n\
# TYPE via_service_condition_degraded_values_total counter\nvia_service_condition_degraded_values_total {}\n\
# TYPE via_service_condition_unavailable_values_total counter\nvia_service_condition_unavailable_values_total {}\n\
# TYPE via_service_condition_not_applicable_values_total counter\nvia_service_condition_not_applicable_values_total {}\n",
            self.cache_hits.load(Ordering::Relaxed),
            self.cache_misses.load(Ordering::Relaxed),
            self.upstream_requests.load(Ordering::Relaxed),
            self.upstream_failures.load(Ordering::Relaxed),
            self.rate_limited_requests.load(Ordering::Relaxed),
            self.fresh_values.load(Ordering::Relaxed),
            self.degraded_values.load(Ordering::Relaxed),
            self.unavailable_values.load(Ordering::Relaxed),
            self.not_applicable_values.load(Ordering::Relaxed),
        )
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ServiceConditionError {
    #[error("service-condition provider timed out")]
    TimedOut,
    #[error("service-condition provider rate limited the request")]
    RateLimited,
    #[error("service-condition provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("service-condition provider returned a rejected payload")]
    RejectedPayload,
    #[error("service-condition provider returned an invalid response")]
    InvalidResponse,
    #[error("service-condition provider configuration is invalid")]
    InvalidConfiguration,
}

impl ServiceConditionError {
    pub fn freshness_reason(&self) -> FreshnessReason {
        match self {
            Self::TimedOut => FreshnessReason::TimedOut,
            Self::RateLimited => FreshnessReason::RateLimited,
            Self::TemporarilyUnavailable | Self::InvalidConfiguration => {
                FreshnessReason::SourceError
            }
            Self::RejectedPayload | Self::InvalidResponse => FreshnessReason::RejectedPayload,
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
pub trait ServiceConditionProvider: Send + Sync {
    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError>;
}

#[derive(Clone)]
pub struct CachedServiceConditionProvider {
    inner: Arc<dyn ServiceConditionProvider>,
    condition_cache: Cache<TransitServiceId, Arc<Vec<NormalizedRealtimeValue>>>,
    ttl: Duration,
    metrics: ServiceConditionMetrics,
}

impl CachedServiceConditionProvider {
    pub fn new(inner: Arc<dyn ServiceConditionProvider>) -> Self {
        Self::with_ttl(inner, DEFAULT_SERVICE_CONDITION_CACHE_TTL)
    }

    pub fn with_ttl(inner: Arc<dyn ServiceConditionProvider>, ttl: Duration) -> Self {
        let ttl = ttl.max(Duration::from_millis(1));
        Self {
            inner,
            condition_cache: Cache::builder().time_to_live(ttl).build(),
            ttl,
            metrics: ServiceConditionMetrics::default(),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn metrics(&self) -> ServiceConditionMetrics {
        self.metrics.clone()
    }

    pub async fn clear(&self) {
        self.condition_cache.invalidate_all();
    }

    async fn service_condition(
        &self,
        service_id: TransitServiceId,
    ) -> Result<Arc<Vec<NormalizedRealtimeValue>>, ServiceConditionError> {
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
impl ServiceConditionProvider for CachedServiceConditionProvider {
    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError> {
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
pub struct NullServiceConditionProvider;

#[async_trait]
impl ServiceConditionProvider for NullServiceConditionProvider {
    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError> {
        Ok(Vec::new())
    }
}
