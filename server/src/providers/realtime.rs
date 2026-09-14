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
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::domain::{
    ids::{BikeShareStationId, BoardingPointId, TransitServiceId},
    realtime::{
        ArrivalEstimate, BikeAvailability, Freshness, FreshnessReason, FreshnessState,
        ServiceCondition,
    },
};

pub const DEFAULT_REALTIME_CACHE_TTL: Duration = Duration::from_secs(45);

#[derive(Clone, Default)]
pub struct RealtimeMetrics {
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
# TYPE via_realtime_upstream_requests_total counter\nvia_realtime_upstream_requests_total {}\n\
# TYPE via_realtime_upstream_failures_total counter\nvia_realtime_upstream_failures_total {}\n\
# TYPE via_realtime_rate_limited_requests_total counter\nvia_realtime_rate_limited_requests_total {}\n",
            self.cache_hits.load(Ordering::Relaxed),
            self.cache_misses.load(Ordering::Relaxed),
            self.upstream_requests.load(Ordering::Relaxed),
            self.upstream_failures.load(Ordering::Relaxed),
            self.rate_limited_requests.load(Ordering::Relaxed),
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
        }
    }

    pub fn for_ride(service_id: TransitServiceId, boarding_point_id: BoardingPointId) -> Self {
        Self::new([service_id], [boarding_point_id])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RealtimeValue {
    ArrivalEstimate(ArrivalEstimate),
    BikeAvailability(BikeAvailability),
    ServiceCondition(ServiceCondition),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRealtimeValue {
    pub id: String,
    pub received_at: OffsetDateTime,
    pub normalized_at: OffsetDateTime,
    pub provider_published_at: Option<OffsetDateTime>,
    pub source_change_at: Option<OffsetDateTime>,
    pub freshness: Freshness,
    pub value: RealtimeValue,
}

impl NormalizedRealtimeValue {
    pub fn kind(&self) -> RealtimeValueKind {
        match self.value {
            RealtimeValue::ArrivalEstimate(_) => RealtimeValueKind::ArrivalEstimate,
            RealtimeValue::BikeAvailability(_) => RealtimeValueKind::BikeAvailability,
            RealtimeValue::ServiceCondition(_) => RealtimeValueKind::ServiceCondition,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeValueKind {
    ArrivalEstimate,
    BikeAvailability,
    ServiceCondition,
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

    async fn bike_availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError>;

    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError>;
}

#[derive(Clone)]
pub struct CachedRealtimeProvider {
    inner: Arc<dyn RealtimeProvider>,
    arrival_cache: Cache<ArrivalQuery, Arc<Vec<NormalizedRealtimeValue>>>,
    bike_cache: Cache<BikeShareStationId, Arc<Vec<NormalizedRealtimeValue>>>,
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
            arrival_cache: Cache::builder().time_to_live(ttl).build(),
            bike_cache: Cache::builder().time_to_live(ttl).build(),
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
        self.arrival_cache.invalidate_all();
        self.bike_cache.invalidate_all();
        self.condition_cache.invalidate_all();
    }

    async fn bike_station(
        &self,
        station_id: BikeShareStationId,
    ) -> Result<Arc<Vec<NormalizedRealtimeValue>>, RealtimeError> {
        let inner = self.inner.clone();
        let key = station_id.clone();
        if self.bike_cache.get(&key).await.is_some() {
            self.metrics.cache_hit();
        } else {
            self.metrics.cache_miss();
        }
        let metrics = self.metrics.clone();
        self.bike_cache
            .try_get_with(key, async move {
                metrics.upstream_request();
                let result = inner
                    .bike_availability(std::slice::from_ref(&station_id))
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
        let inner = self.inner.clone();
        if self.arrival_cache.get(&query).await.is_some() {
            self.metrics.cache_hit();
        } else {
            self.metrics.cache_miss();
        }
        let metrics = self.metrics.clone();
        self.arrival_cache
            .try_get_with(query.clone(), async move {
                metrics.upstream_request();
                let result = inner.arrival_estimates(query).await.map(Arc::new);
                if let Ok(values) = &result {
                    metrics.observe_values(values);
                }
                metrics.upstream_result(result.as_ref().err());
                result
            })
            .await
            .map(|values| values.as_ref().clone())
            .map_err(|error| (*error).clone())
    }

    async fn bike_availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        let mut unique = HashSet::new();
        let results = try_join_all(
            station_ids
                .iter()
                .filter(|station_id| unique.insert((*station_id).clone()))
                .cloned()
                .map(|station_id| self.bike_station(station_id)),
        )
        .await?;
        Ok(results
            .into_iter()
            .flat_map(|values| values.as_ref().clone())
            .collect())
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

    async fn bike_availability(
        &self,
        _station_ids: &[BikeShareStationId],
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
