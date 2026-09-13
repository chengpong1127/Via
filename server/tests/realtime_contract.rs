use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures_util::future::join_all;
use time::OffsetDateTime;
use via_server::{
    domain::{
        ids::{BikeShareStationId, BoardingPointId, TransitServiceId},
        realtime::{ArrivalEstimate, ArrivalState, Freshness, FreshnessState},
    },
    providers::realtime::{
        ArrivalQuery, CachedRealtimeProvider, NormalizedRealtimeValue, RealtimeError,
        RealtimeProvider, RealtimeValue,
    },
};

#[derive(Clone)]
struct CountingProvider {
    calls: Arc<AtomicUsize>,
    result: Result<Vec<NormalizedRealtimeValue>, RealtimeError>,
    delay_ms: u64,
}

#[async_trait]
impl RealtimeProvider for CountingProvider {
    async fn arrival_estimates(
        &self,
        _query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        self.result.clone()
    }

    async fn bike_availability(
        &self,
        _station_ids: &[BikeShareStationId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }

    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

fn arrival_value(freshness: Freshness) -> NormalizedRealtimeValue {
    let now = OffsetDateTime::now_utc();
    NormalizedRealtimeValue {
        id: "arrival-1".into(),
        received_at: now,
        normalized_at: now,
        provider_published_at: Some(now),
        source_change_at: None,
        freshness,
        value: RealtimeValue::ArrivalEstimate(ArrivalEstimate {
            transit_service_id: TransitServiceId::new("service-1"),
            boarding_point_id: BoardingPointId::new("stop-1"),
            predicted_arrival_at: Some(now + time::Duration::seconds(120)),
            state: ArrivalState::Predicted,
            transmitted_at: now,
        }),
    }
}

fn query() -> ArrivalQuery {
    ArrivalQuery::for_ride(
        TransitServiceId::new("service-1"),
        BoardingPointId::new("stop-1"),
    )
}

#[tokio::test]
async fn cache_hit_avoids_another_upstream_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: calls.clone(),
        result: Ok(vec![arrival_value(Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: OffsetDateTime::now_utc(),
        })]),
        delay_ms: 0,
    };
    let cached =
        CachedRealtimeProvider::with_ttl(Arc::new(provider), std::time::Duration::from_secs(30));
    cached.arrival_estimates(query()).await.unwrap();
    cached.arrival_estimates(query()).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn concurrent_cache_misses_are_coalesced() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: calls.clone(),
        result: Ok(vec![arrival_value(Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: OffsetDateTime::now_utc(),
        })]),
        delay_ms: 25,
    };
    let cached = Arc::new(CachedRealtimeProvider::with_ttl(
        Arc::new(provider),
        std::time::Duration::from_secs(30),
    ));
    let results = join_all((0..50).map(|_| {
        let cached = cached.clone();
        async move { cached.arrival_estimates(query()).await }
    }))
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn ttl_expiry_fetches_again_but_errors_are_not_cached() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: calls.clone(),
        result: Ok(vec![arrival_value(Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: OffsetDateTime::now_utc(),
        })]),
        delay_ms: 0,
    };
    let cached =
        CachedRealtimeProvider::with_ttl(Arc::new(provider), std::time::Duration::from_millis(10));
    cached.arrival_estimates(query()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    cached.arrival_estimates(query()).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    let failing = CountingProvider {
        calls: calls.clone(),
        result: Err(RealtimeError::TimedOut),
        delay_ms: 0,
    };
    let cached =
        CachedRealtimeProvider::with_ttl(Arc::new(failing), std::time::Duration::from_secs(30));
    assert_eq!(
        cached.arrival_estimates(query()).await.unwrap_err(),
        RealtimeError::TimedOut
    );
    assert_eq!(
        cached.arrival_estimates(query()).await.unwrap_err(),
        RealtimeError::TimedOut
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
}

#[tokio::test]
async fn recently_cached_stale_values_remain_unavailable() {
    let now = OffsetDateTime::now_utc();
    let provider = CountingProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        result: Ok(vec![arrival_value(Freshness {
            state: FreshnessState::Unavailable,
            reason: Some(via_server::domain::realtime::FreshnessReason::Stale),
            policy_version: "1".into(),
            evaluated_at: now,
        })]),
        delay_ms: 0,
    };
    let cached = CachedRealtimeProvider::new(Arc::new(provider));
    let value = cached.arrival_estimates(query()).await.unwrap().remove(0);
    assert_eq!(value.freshness.state, FreshnessState::Unavailable);
}
