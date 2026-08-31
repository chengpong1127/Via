use time::OffsetDateTime;
use via_server::{
    live_status::{Freshness, FreshnessState},
    normalization::{ArrivalEstimate, ArrivalState, BoardingPointId, TransitServiceId},
    postgres::PostgresSnapshotStore,
    snapshot::{LiveStatusValue, NetworkSnapshot, NormalizedLiveStatus, SnapshotStore},
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
        live_status: Vec::new(),
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
    let mut next = snapshot(
        format!("integration-snapshot-{}", now.unix_timestamp_nanos()),
        current.publication_version + 1,
        now,
    );
    next.live_status.push(NormalizedLiveStatus {
        id: format!("integration-observation-{}", now.unix_timestamp_nanos()),
        received_at: now,
        normalized_at: now,
        freshness: Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: now,
        },
        value: LiveStatusValue::ArrivalEstimate(ArrivalEstimate {
            transit_service_id: TransitServiceId::new("via_service_test"),
            boarding_point_id: BoardingPointId::new("via_boarding_point_test"),
            predicted_arrival_at: Some(now),
            state: ArrivalState::Predicted,
            transmitted_at: now,
        }),
    });

    store
        .publish(lease, current.publication_version, next.clone())
        .await
        .unwrap();

    let published = store.pin_current().await.unwrap();
    assert_eq!(published, next);

    let reloaded_store = PostgresSnapshotStore::connect(&database_url, 2)
        .await
        .unwrap();
    assert_eq!(reloaded_store.pin_current().await.unwrap(), next);
}
