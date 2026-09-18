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
    ids::{BoardingPointId, TransitServiceId},
    realtime::{FreshnessReason, FreshnessState},
};

pub use crate::domain::realtime::{NormalizedRealtimeValue, RealtimeValue, RealtimeValueKind};

pub const DEFAULT_REALTIME_CACHE_TTL: Duration = Duration::from_secs(45);

#[derive(Clone, Default)]
pub struct RealtimeMetrics {
    cache_hits: Arc<AtomicU64>,
    cache_misses: Arc<AtomicU64>,
    arrival_cache_hits: Arc<AtomicU64>,
    arrival_cache_misses: Arc<AtomicU64>,
    upstream_requests: Arc<AtomicU64>,
    arrival_upstream_requests: Arc<AtomicU64>,
    upstream_failures: Arc<AtomicU64>,
    rate_limited_requests: Arc<AtomicU64>,
    arrival_rate_limited_requests: Arc<AtomicU64>,
    logical_departure_needs: Arc<AtomicU64>,
    deduplicated_departure_needs: Arc<AtomicU64>,
    departure_batches: Arc<AtomicU64>,
    departure_batch_size: Arc<AtomicU64>,
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

    fn arrival_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
        self.arrival_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn arrival_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
        self.arrival_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    fn upstream_request(&self) {
        self.upstream_requests.fetch_add(1, Ordering::Relaxed);
    }

    fn arrival_upstream_request(&self) {
        self.arrival_upstream_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    fn upstream_result(&self, error: Option<&RealtimeError>) {
        if let Some(error) = error {
            self.upstream_failures.fetch_add(1, Ordering::Relaxed);
            if matches!(error, RealtimeError::RateLimited) {
                self.rate_limited_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn arrival_upstream_result(&self, error: Option<&RealtimeError>) {
        if matches!(error, Some(RealtimeError::RateLimited)) {
            self.arrival_rate_limited_requests
                .fetch_add(1, Ordering::Relaxed);
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
        self.departure_batch_size
            .fetch_add(unique_needs as u64, Ordering::Relaxed);
    }

    pub fn bus_timing_trace(&self) -> String {
        format!(
            "BUS TIMING\n  logical needs={}\n  unique needs={}\n  upstream batches={}\n  cache hits={}\n  upstream calls={}",
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.deduplicated_departure_needs.load(Ordering::Relaxed),
            self.departure_batches.load(Ordering::Relaxed),
            self.arrival_cache_hits.load(Ordering::Relaxed),
            self.arrival_upstream_requests.load(Ordering::Relaxed),
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
        let mut output = format!(
            "# TYPE via_realtime_cache_hits_total counter\nvia_realtime_cache_hits_total {}\n\
# TYPE via_realtime_cache_misses_total counter\nvia_realtime_cache_misses_total {}\n\
# TYPE via_realtime_arrival_cache_hits_total counter\nvia_realtime_arrival_cache_hits_total {}\n\
# TYPE via_realtime_arrival_cache_misses_total counter\nvia_realtime_arrival_cache_misses_total {}\n\
# TYPE via_realtime_upstream_requests_total counter\nvia_realtime_upstream_requests_total {}\n\
# TYPE via_realtime_arrival_upstream_requests_total counter\nvia_realtime_arrival_upstream_requests_total {}\n\
# TYPE via_realtime_upstream_failures_total counter\nvia_realtime_upstream_failures_total {}\n\
# TYPE via_realtime_rate_limited_requests_total counter\nvia_realtime_rate_limited_requests_total {}\n\
# TYPE via_realtime_arrival_rate_limited_requests_total counter\nvia_realtime_arrival_rate_limited_requests_total {}\n\
# TYPE via_realtime_logical_departure_needs_total counter\nvia_realtime_logical_departure_needs_total {}\n\
# TYPE via_realtime_deduplicated_departure_needs_total counter\nvia_realtime_deduplicated_departure_needs_total {}\n\
# TYPE via_realtime_departure_batches_total counter\nvia_realtime_departure_batches_total {}\n\
# TYPE via_realtime_departure_batch_size_total counter\nvia_realtime_departure_batch_size_total {}\n",
            self.cache_hits.load(Ordering::Relaxed),
            self.cache_misses.load(Ordering::Relaxed),
            self.arrival_cache_hits.load(Ordering::Relaxed),
            self.arrival_cache_misses.load(Ordering::Relaxed),
            self.upstream_requests.load(Ordering::Relaxed),
            self.arrival_upstream_requests.load(Ordering::Relaxed),
            self.upstream_failures.load(Ordering::Relaxed),
            self.rate_limited_requests.load(Ordering::Relaxed),
            self.arrival_rate_limited_requests.load(Ordering::Relaxed),
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.deduplicated_departure_needs.load(Ordering::Relaxed),
            self.departure_batches.load(Ordering::Relaxed),
            self.departure_batch_size.load(Ordering::Relaxed),
        );
        output.push_str(&format!(
            "# TYPE via_realtime_fresh_values_total counter\nvia_realtime_fresh_values_total {}\n\
# TYPE via_realtime_degraded_values_total counter\nvia_realtime_degraded_values_total {}\n\
# TYPE via_realtime_unavailable_values_total counter\nvia_realtime_unavailable_values_total {}\n\
# TYPE via_realtime_not_applicable_values_total counter\nvia_realtime_not_applicable_values_total {}\n",
            self.fresh_values.load(Ordering::Relaxed),
            self.degraded_values.load(Ordering::Relaxed),
            self.unavailable_values.load(Ordering::Relaxed),
            self.not_applicable_values.load(Ordering::Relaxed),
        ));
        output
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArrivalQuery {
    pub transit_service_ids: Vec<TransitServiceId>,
    pub boarding_point_ids: Vec<BoardingPointId>,
    pub scope: Option<String>,
}

impl ArrivalQuery {
    pub fn new(
        transit_service_ids: impl IntoIterator<Item = TransitServiceId>,
        boarding_point_ids: impl IntoIterator<Item = BoardingPointId>,
    ) -> Self {
        let mut transit_service_ids = transit_service_ids.into_iter().collect::<Vec<_>>();
        let mut boarding_point_ids = boarding_point_ids.into_iter().collect::<Vec<_>>();
        transit_service_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        boarding_point_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        transit_service_ids.dedup();
        boarding_point_ids.dedup();
        Self {
            transit_service_ids,
            boarding_point_ids,
            scope: None,
        }
    }

    pub fn for_ride(service_id: TransitServiceId, boarding_point_id: BoardingPointId) -> Self {
        Self::new([service_id], [boarding_point_id])
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    pub fn with_optional_scope(mut self, scope: Option<String>) -> Self {
        self.scope = scope;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ArrivalPair {
    service_id: TransitServiceId,
    boarding_point_id: BoardingPointId,
    scope: Option<String>,
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
    async fn arrival_estimates(
        &self,
        query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError>;

    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError>;
}

#[derive(Clone)]
pub struct CachedRealtimeProvider {
    inner: Arc<dyn RealtimeProvider>,
    arrival_batch_cache: Cache<ArrivalQuery, Arc<Vec<NormalizedRealtimeValue>>>,
    arrival_pair_cache: Cache<ArrivalPair, Arc<Vec<NormalizedRealtimeValue>>>,
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
            arrival_batch_cache: Cache::builder().time_to_live(ttl).build(),
            arrival_pair_cache: Cache::builder().time_to_live(ttl).build(),
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
        self.arrival_batch_cache.invalidate_all();
        self.arrival_pair_cache.invalidate_all();
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
    async fn arrival_estimates(
        &self,
        query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        if query.transit_service_ids.is_empty() || query.boarding_point_ids.is_empty() {
            return Ok(Vec::new());
        }
        let pairs = query
            .transit_service_ids
            .iter()
            .flat_map(|service_id| {
                query
                    .boarding_point_ids
                    .iter()
                    .map(|boarding_point_id| ArrivalPair {
                        service_id: service_id.clone(),
                        boarding_point_id: boarding_point_id.clone(),
                        scope: query.scope.clone(),
                    })
            })
            .collect::<Vec<_>>();
        let inner = self.inner.clone();
        if self.arrival_batch_cache.get(&query).await.is_some() {
            self.metrics.arrival_cache_hit();
        } else {
            let mut all_pairs_cached = true;
            for pair in &pairs {
                if self.arrival_pair_cache.get(pair).await.is_none() {
                    all_pairs_cached = false;
                    break;
                }
            }
            if all_pairs_cached {
                self.metrics.arrival_cache_hit();
            } else {
                self.metrics.arrival_cache_miss();
            }
        }
        let arrival_pair_cache = self.arrival_pair_cache.clone();
        let metrics = self.metrics.clone();
        self.arrival_batch_cache
            .try_get_with(query.clone(), async move {
                let mut values = Vec::new();
                let mut missing_pairs = Vec::new();
                for pair in &pairs {
                    if let Some(cached) = arrival_pair_cache.get(pair).await {
                        values.extend(cached.as_ref().iter().cloned());
                    } else {
                        missing_pairs.push(pair.clone());
                    }
                }

                if !missing_pairs.is_empty() {
                    let fetch_query = ArrivalQuery::new(
                        missing_pairs.iter().map(|pair| pair.service_id.clone()),
                        missing_pairs
                            .iter()
                            .map(|pair| pair.boarding_point_id.clone()),
                    )
                    .with_optional_scope(query.scope.clone());
                    metrics.upstream_request();
                    metrics.arrival_upstream_request();
                    let result = inner.arrival_estimates(fetch_query).await;
                    metrics.arrival_upstream_result(result.as_ref().err());
                    metrics.upstream_result(result.as_ref().err());
                    let fetched = match result {
                        Ok(values) => values,
                        Err(error) => return Err(error),
                    };
                    metrics.observe_values(&fetched);
                    for pair in missing_pairs {
                        let pair_values = fetched
                            .iter()
                            .filter(|value| arrival_matches_pair(value, &pair))
                            .cloned()
                            .collect::<Vec<_>>();
                        arrival_pair_cache
                            .insert(pair, Arc::new(pair_values.clone()))
                            .await;
                        values.extend(pair_values);
                    }
                }

                Ok(Arc::new(values))
            })
            .await
            .map(|values| values.as_ref().clone())
            .map_err(|error| (*error).clone())
    }

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

fn arrival_matches_pair(value: &NormalizedRealtimeValue, pair: &ArrivalPair) -> bool {
    matches!(
        &value.value,
        RealtimeValue::ArrivalEstimate(arrival)
            if arrival.transit_service_id == pair.service_id
                && arrival.boarding_point_id == pair.boarding_point_id
    )
}

#[derive(Default)]
pub struct NullRealtimeProvider;

#[async_trait]
impl RealtimeProvider for NullRealtimeProvider {
    async fn arrival_estimates(
        &self,
        _query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        Ok(Vec::new())
    }

    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        Ok(Vec::new())
    }
}
