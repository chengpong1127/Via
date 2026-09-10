use std::sync::Arc;

use async_trait::async_trait;
use time::macros::datetime;
use via_server::{
    live_status::{Freshness, FreshnessPolicy, FreshnessReason, FreshnessState},
    normalization::{
        ArrivalEstimate, ArrivalState, BoardingPointId, ServiceCondition, ServiceConditionState,
        TransitServiceId,
    },
    refresh::{FeedSource, RefreshCoordinator},
    snapshot::{
        InMemorySnapshotStore, LiveStatusValue, NetworkSnapshot, NormalizedLiveStatus,
        PublishError, SnapshotStore,
    },
};

fn snapshot(id: &str, version: u64) -> NetworkSnapshot {
    NetworkSnapshot {
        id: id.into(),
        created_at: datetime!(2026-08-31 12:00 +08:00),
        live_cutoff: datetime!(2026-08-31 11:59:50 +08:00),
        static_network_revision: "static-1".into(),
        schedule_revision: "schedule-1".into(),
        freshness_policy_version: "1".into(),
        publication_version: version,
        feed_evidence: Vec::new(),
        live_status: Vec::new(),
    }
}

#[tokio::test]
async fn overlapping_refreshes_receive_at_most_one_lease() {
    let store = Arc::new(InMemorySnapshotStore::new(snapshot("initial", 1)));
    let first_store = store.clone();
    let second_store = store.clone();
    let now = datetime!(2026-08-31 12:00 +08:00);

    let (first, second) = tokio::join!(
        first_store.acquire_refresh_lease("cycle-a", now),
        second_store.acquire_refresh_lease("cycle-b", now)
    );

    assert_ne!(first.unwrap().is_some(), second.unwrap().is_some());
}

#[tokio::test]
async fn retrying_a_published_refresh_key_is_idempotent() {
    let store = InMemorySnapshotStore::new(snapshot("initial", 1));
    let now = datetime!(2026-08-31 12:00 +08:00);
    let lease = store
        .acquire_refresh_lease("cycle-a", now)
        .await
        .unwrap()
        .unwrap();
    store.publish(lease, 1, snapshot("next", 2)).await.unwrap();

    assert!(
        store
            .acquire_refresh_lease("cycle-a", now)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(store.pin_current().await.unwrap().id, "next");
}

#[tokio::test]
async fn stale_publication_version_cannot_replace_a_newer_snapshot() {
    let store = InMemorySnapshotStore::new(snapshot("initial", 3));
    let now = datetime!(2026-08-31 12:00 +08:00);
    let lease = store
        .acquire_refresh_lease("cycle-a", now)
        .await
        .unwrap()
        .unwrap();
    let error = store
        .publish(lease, 2, snapshot("stale", 3))
        .await
        .unwrap_err();

    assert_eq!(error, PublishError::PublicationVersionChanged);
    assert_eq!(store.pin_current().await.unwrap().id, "initial");
}

#[tokio::test]
async fn losing_the_warm_cache_does_not_change_the_pinned_snapshot() {
    let store = InMemorySnapshotStore::new(snapshot("initial", 1));
    let before = store.pin_current().await.unwrap();
    store.clear_cache().await;
    let after = store.pin_current().await.unwrap();

    assert_eq!(before, after);
}

struct FailedBusFeed;

#[async_trait]
impl FeedSource for FailedBusFeed {
    fn capability(&self) -> &str {
        "bus_arrival_taipei"
    }

    async fn refresh(
        &self,
        _: time::OffsetDateTime,
    ) -> Result<via_server::refresh::FeedRefreshBatch, FreshnessReason> {
        Err(FreshnessReason::RateLimited)
    }
}

#[tokio::test]
async fn failed_feed_retains_previous_observation_but_never_keeps_it_fresh() {
    let now = datetime!(2026-08-31 12:00 +08:00);
    let mut initial = snapshot("initial", 1);
    initial.live_status.push(NormalizedLiveStatus {
        id: "previous-bus-observation".into(),
        capability: "bus_arrival_taipei".into(),
        received_at: now - time::Duration::seconds(30),
        normalized_at: now - time::Duration::seconds(30),
        freshness: Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: now - time::Duration::seconds(30),
        },
        value: LiveStatusValue::ArrivalEstimate(ArrivalEstimate {
            transit_service_id: TransitServiceId::new("service-1"),
            boarding_point_id: BoardingPointId::new("stop-1"),
            predicted_arrival_at: Some(now + time::Duration::seconds(120)),
            state: ArrivalState::Predicted,
            transmitted_at: now - time::Duration::seconds(30),
        }),
    });
    let store = Arc::new(InMemorySnapshotStore::new(initial));
    let coordinator = RefreshCoordinator::new(store.clone(), vec![Arc::new(FailedBusFeed)], 1);

    coordinator.run("failed-bus-refresh", now).await.unwrap();

    let refreshed = store.pin_current().await.unwrap();
    assert_eq!(refreshed.live_status.len(), 1);
    assert_ne!(refreshed.live_status[0].id, "previous-bus-observation");
    assert_eq!(
        refreshed.live_status[0].freshness.state,
        FreshnessState::Unavailable
    );
    assert_eq!(
        refreshed.live_status[0].freshness.reason,
        Some(FreshnessReason::RateLimited)
    );
    assert_eq!(
        refreshed.feed_evidence[0].failure_reason,
        Some(FreshnessReason::RateLimited)
    );
    assert_eq!(refreshed.live_status[0].freshness.evaluated_at, now);
}

struct SlowConditionFeed;

#[async_trait]
impl FeedSource for SlowConditionFeed {
    fn capability(&self) -> &str {
        "metro_condition_taipei"
    }

    fn freshness_policy(&self, version: &str) -> FreshnessPolicy {
        FreshnessPolicy {
            version: version.into(),
            fresh_through: time::Duration::minutes(10),
            degraded_through: time::Duration::minutes(15),
        }
    }

    async fn refresh(
        &self,
        _: time::OffsetDateTime,
    ) -> Result<via_server::refresh::FeedRefreshBatch, FreshnessReason> {
        Ok(via_server::refresh::FeedRefreshBatch {
            freshness: Freshness {
                state: FreshnessState::Fresh,
                reason: None,
                policy_version: "1".into(),
                evaluated_at: datetime!(2026-08-31 12:00 +08:00),
            },
            live_status: Vec::new(),
        })
    }
}

#[tokio::test]
async fn retained_observations_use_the_source_specific_freshness_policy() {
    let now = datetime!(2026-08-31 12:00 +08:00);
    let mut initial = snapshot("initial", 1);
    initial.live_status.push(NormalizedLiveStatus {
        id: "previous-metro-condition".into(),
        capability: "metro_condition_taipei".into(),
        received_at: now - time::Duration::minutes(5),
        normalized_at: now - time::Duration::minutes(5),
        freshness: Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: now - time::Duration::minutes(5),
        },
        value: LiveStatusValue::ServiceCondition(ServiceCondition {
            state: ServiceConditionState::Disrupted,
            scope_via_ids: vec!["service-1".into()],
            source_text_variants: Vec::new(),
            effective_from: None,
            effective_until: None,
        }),
    });
    let store = Arc::new(InMemorySnapshotStore::new(initial));
    let coordinator = RefreshCoordinator::new(store.clone(), vec![Arc::new(SlowConditionFeed)], 1);

    coordinator
        .run("slow-condition-refresh", now)
        .await
        .unwrap();

    let refreshed = store.pin_current().await.unwrap();
    assert_eq!(
        refreshed.live_status[0].freshness.state,
        FreshnessState::Fresh
    );
    assert_eq!(refreshed.live_status[0].freshness.policy_version, "1");
}
