use std::sync::Arc;

use time::macros::datetime;
use via_server::snapshot::{InMemorySnapshotStore, NetworkSnapshot, PublishError, SnapshotStore};

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
