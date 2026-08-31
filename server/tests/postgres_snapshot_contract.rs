use time::OffsetDateTime;
use via_server::{
    postgres::PostgresSnapshotStore,
    snapshot::{NetworkSnapshot, SnapshotStore},
};

fn snapshot(id: String, version: u64, now: OffsetDateTime) -> NetworkSnapshot {
    NetworkSnapshot {
        id,
        created_at: now,
        live_cutoff: now,
        static_network_revision: "test-static".into(),
        schedule_revision: "test-schedule".into(),
        freshness_policy_version: "1".into(),
        publication_version: version,
        feed_evidence: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "requires VIA_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn postgres_is_authoritative_for_lease_and_snapshot_publication() {
    let database_url = std::env::var("VIA_TEST_DATABASE_URL").unwrap();
    let store = PostgresSnapshotStore::connect(&database_url, 2)
        .await
        .unwrap();
    let now = OffsetDateTime::now_utc();
    store
        .ensure_initial(snapshot("integration-initial".into(), 1, now))
        .await
        .unwrap();
    let current = store.pin_current().await.unwrap();
    let refresh_key = format!("integration-{}", now.unix_timestamp_nanos());
    let lease = store
        .acquire_refresh_lease(&refresh_key, now)
        .await
        .unwrap()
        .unwrap();
    let next = snapshot(
        format!("integration-snapshot-{}", now.unix_timestamp_nanos()),
        current.publication_version + 1,
        now,
    );

    store
        .publish(lease, current.publication_version, next.clone())
        .await
        .unwrap();

    assert_eq!(store.pin_current().await.unwrap(), next);
}
