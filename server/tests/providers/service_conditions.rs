use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures_util::future::join_all;
use time::OffsetDateTime;
use via_server::{
    domain::{
        ids::TransitPatternId,
        realtime::{
            Freshness, FreshnessState, NormalizedRealtimeValue, RealtimeValue, ServiceCondition,
            ServiceConditionState,
        },
    },
    providers::service_conditions::{
        CachedServiceConditionProvider, ServiceConditionError, ServiceConditionProvider,
    },
};

#[derive(Clone)]
struct CountingProvider {
    calls: Arc<AtomicUsize>,
    result: Result<Vec<NormalizedRealtimeValue>, ServiceConditionError>,
    delay_ms: u64,
}

#[async_trait]
impl ServiceConditionProvider for CountingProvider {
    async fn service_conditions(
        &self,
        _service_ids: &[TransitPatternId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        self.result.clone()
    }
}

fn condition_value(freshness: Freshness) -> NormalizedRealtimeValue {
    let now = OffsetDateTime::now_utc();
    NormalizedRealtimeValue {
        id: "condition-1".into(),
        received_at: now,
        normalized_at: now,
        provider_published_at: Some(now),
        source_change_at: None,
        freshness,
        value: RealtimeValue::ServiceCondition(ServiceCondition {
            state: ServiceConditionState::Disrupted,
            scope_via_ids: vec!["service-1".into()],
            source_text_variants: Vec::new(),
            effective_from: None,
            effective_until: None,
        }),
    }
}

fn services() -> Vec<TransitPatternId> {
    vec![TransitPatternId::new("service-1")]
}

fn fresh_value() -> NormalizedRealtimeValue {
    condition_value(Freshness {
        state: FreshnessState::Fresh,
        reason: None,
        policy_version: "1".into(),
        evaluated_at: OffsetDateTime::now_utc(),
    })
}

#[tokio::test]
async fn service_condition_cache_hit_avoids_another_upstream_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: calls.clone(),
        result: Ok(vec![fresh_value()]),
        delay_ms: 0,
    };
    let cached = CachedServiceConditionProvider::with_ttl(
        Arc::new(provider),
        std::time::Duration::from_secs(30),
    );
    cached.service_conditions(&services()).await.unwrap();
    cached.service_conditions(&services()).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn concurrent_service_condition_misses_are_coalesced() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: calls.clone(),
        result: Ok(vec![fresh_value()]),
        delay_ms: 25,
    };
    let cached = Arc::new(CachedServiceConditionProvider::with_ttl(
        Arc::new(provider),
        std::time::Duration::from_secs(30),
    ));
    let results = join_all((0..50).map(|_| {
        let cached = cached.clone();
        async move { cached.service_conditions(&services()).await }
    }))
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn service_condition_ttl_expiry_fetches_again_but_errors_are_not_cached() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        calls: calls.clone(),
        result: Ok(vec![fresh_value()]),
        delay_ms: 0,
    };
    let cached = CachedServiceConditionProvider::with_ttl(
        Arc::new(provider),
        std::time::Duration::from_millis(10),
    );
    cached.service_conditions(&services()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    cached.service_conditions(&services()).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    let failing = CountingProvider {
        calls: calls.clone(),
        result: Err(ServiceConditionError::TimedOut),
        delay_ms: 0,
    };
    let cached = CachedServiceConditionProvider::with_ttl(
        Arc::new(failing),
        std::time::Duration::from_secs(30),
    );
    assert_eq!(
        cached.service_conditions(&services()).await.unwrap_err(),
        ServiceConditionError::TimedOut
    );
    assert_eq!(
        cached.service_conditions(&services()).await.unwrap_err(),
        ServiceConditionError::TimedOut
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
}

#[tokio::test]
async fn recently_cached_stale_values_remain_unavailable() {
    let now = OffsetDateTime::now_utc();
    let provider = CountingProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        result: Ok(vec![condition_value(Freshness {
            state: FreshnessState::Unavailable,
            reason: Some(via_server::domain::realtime::FreshnessReason::Stale),
            policy_version: "1".into(),
            evaluated_at: now,
        })]),
        delay_ms: 0,
    };
    let cached = CachedServiceConditionProvider::new(Arc::new(provider));
    let value = cached
        .service_conditions(&services())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(value.freshness.state, FreshnessState::Unavailable);
}
