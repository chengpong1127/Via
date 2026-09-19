use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::support::tdx::spawn_tdx_fixture;
use axum::{Json, Router, http::StatusCode, routing::get};
use futures_util::future::join_all;
use serde_json::json;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    domain::{
        bikeshare::{BikeShareNetwork, BikeStation, BikeStationAccess, BikeWalkingAccess},
        ids::{BikeStationId, BoardingPointId},
        location::RequestCoordinate,
    },
    providers::{BikeAvailabilityError, BikeAvailabilitySource, TdxBikeAvailabilitySource},
};

fn station(id: &str) -> BikeStation {
    BikeStation::new(
        BikeStationId::new(id),
        id,
        RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    )
}

fn network() -> Arc<BikeShareNetwork> {
    Arc::new(
        BikeShareNetwork::new(vec![
            station("tdx:Taipei:TPE-1"),
            station("tdx:Taipei:TPE-DISABLED"),
            station("tdx:Taipei:TPE-STALE"),
            station("tdx:NewTaipei:NWT-1"),
        ])
        .with_access_links(vec![
            BikeStationAccess::new(
                BoardingPointId::new("pickup"),
                BikeStationId::new("tdx:Taipei:TPE-1"),
                Some(BikeWalkingAccess::new(20, 100)),
                None,
            ),
            BikeStationAccess::new(
                BoardingPointId::new("dropoff"),
                BikeStationId::new("tdx:Taipei:TPE-1"),
                None,
                Some(BikeWalkingAccess::new(30, 150)),
            ),
            BikeStationAccess::new(
                BoardingPointId::new("pickup"),
                BikeStationId::new("tdx:NewTaipei:NWT-1"),
                Some(BikeWalkingAccess::new(10, 80)),
                None,
            ),
            BikeStationAccess::new(
                BoardingPointId::new("dropoff"),
                BikeStationId::new("tdx:NewTaipei:NWT-1"),
                None,
                Some(BikeWalkingAccess::new(10, 80)),
            ),
            BikeStationAccess::new(
                BoardingPointId::new("disabled"),
                BikeStationId::new("tdx:Taipei:TPE-DISABLED"),
                Some(BikeWalkingAccess::new(10, 80)),
                Some(BikeWalkingAccess::new(10, 80)),
            ),
            BikeStationAccess::new(
                BoardingPointId::new("stale"),
                BikeStationId::new("tdx:Taipei:TPE-STALE"),
                Some(BikeWalkingAccess::new(10, 80)),
                Some(BikeWalkingAccess::new(10, 80)),
            ),
        ])
        .with_revision("revision"),
    )
}

fn timestamp(age: TimeDuration) -> String {
    (OffsetDateTime::now_utc() - age).format(&Rfc3339).unwrap()
}

fn availability(
    station_uid: &str,
    service_status: i32,
    available_bikes: i32,
    available_return_slots: i32,
    age: TimeDuration,
) -> serde_json::Value {
    json!({
        "StationUID": station_uid,
        "ServiceStatus": service_status,
        "AvailableRentBikes": available_bikes,
        "AvailableReturnBikes": available_return_slots,
        "SrcUpdateTime": timestamp(age),
        "UpdateTime": timestamp(age),
    })
}

async fn source_with_routes(
    taipei: serde_json::Value,
    new_taipei: serde_json::Value,
) -> TdxBikeAvailabilitySource {
    let app = Router::new()
        .route(
            "/v2/Bike/Availability/City/Taipei",
            get(move || {
                let payload = taipei.clone();
                async move { Json(payload) }
            }),
        )
        .route(
            "/v2/Bike/Availability/City/NewTaipei",
            get(move || {
                let payload = new_taipei.clone();
                async move { Json(payload) }
            }),
        )
        .route(
            "/",
            axum::routing::post(|| async {
                Json(json!({"access_token": "fixture-token", "expires_in": 300}))
            }),
        );
    let client = spawn_tdx_fixture(app).await;
    TdxBikeAvailabilitySource::new(network(), client)
}

#[tokio::test]
async fn merges_taipei_and_new_taipei_with_canonical_ids_and_counts() {
    let source = source_with_routes(
        json!([
            availability("TPE-1", 1, 7, 2, TimeDuration::seconds(10)),
            availability("TPE-UNKNOWN", 1, 99, 99, TimeDuration::seconds(10)),
        ]),
        json!([availability("NWT-1", 1, 0, 3, TimeDuration::seconds(10),)]),
    )
    .await;

    let snapshot = source.snapshot().await.unwrap();
    assert_eq!(snapshot.stations.len(), 2);
    let taipei = snapshot
        .station(&BikeStationId::new("tdx:Taipei:TPE-1"))
        .unwrap();
    assert_eq!(taipei.available_bikes, 7);
    assert_eq!(taipei.available_return_slots, 2);
    assert!(taipei.in_service);
    let new_taipei = snapshot
        .station(&BikeStationId::new("tdx:NewTaipei:NWT-1"))
        .unwrap();
    assert_eq!(new_taipei.available_bikes, 0);
    assert_eq!(new_taipei.available_return_slots, 3);
    assert!(
        snapshot
            .station(&BikeStationId::new("tdx:Taipei:TPE-UNKNOWN"))
            .is_none()
    );
}

#[tokio::test]
async fn unusable_counts_status_stale_observations_and_directionality_are_filtered() {
    let source = source_with_routes(
        json!([
            availability("TPE-1", 1, 7, 2, TimeDuration::seconds(10)),
            availability("TPE-DISABLED", 2, 7, 7, TimeDuration::seconds(10)),
            availability("TPE-STALE", 1, 7, 7, TimeDuration::seconds(121)),
        ]),
        json!([availability("NWT-1", 1, 0, 0, TimeDuration::seconds(10),)]),
    )
    .await;
    let snapshot = source.snapshot().await.unwrap();
    let network = network();
    let pickup = network
        .availability_view(&snapshot)
        .usable_pickups(&BoardingPointId::new("pickup"));
    assert_eq!(pickup.len(), 1);
    assert_eq!(pickup[0].station_id.as_str(), "tdx:Taipei:TPE-1");
    let dropoff = network
        .availability_view(&snapshot)
        .usable_dropoffs(&BoardingPointId::new("dropoff"));
    assert_eq!(dropoff.len(), 1);
    assert_eq!(dropoff[0].station_id.as_str(), "tdx:Taipei:TPE-1");
    assert!(
        network
            .availability_view(&snapshot)
            .usable_pickups(&BoardingPointId::new("disabled"))
            .is_empty()
    );
    assert!(
        network
            .availability_view(&snapshot)
            .usable_pickups(&BoardingPointId::new("stale"))
            .is_empty()
    );
    assert_eq!(
        network
            .availability_view(&snapshot)
            .usable_dropoffs(&BoardingPointId::new("dropoff"))[0]
            .walk_from_station,
        Some(BikeWalkingAccess::new(30, 150))
    );
}

#[tokio::test]
async fn fresh_snapshot_is_cached_and_expiry_refreshes_both_city_requests() {
    let taipei_calls = Arc::new(AtomicUsize::new(0));
    let new_taipei_calls = Arc::new(AtomicUsize::new(0));
    let taipei_counter = taipei_calls.clone();
    let new_taipei_counter = new_taipei_calls.clone();
    let app = Router::new()
        .route(
            "/v2/Bike/Availability/City/Taipei",
            get(move || {
                let counter = taipei_counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    Json(json!([availability(
                        "TPE-1",
                        1,
                        1,
                        1,
                        TimeDuration::seconds(10),
                    )]))
                }
            }),
        )
        .route(
            "/v2/Bike/Availability/City/NewTaipei",
            get(move || {
                let counter = new_taipei_counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    Json(json!([]))
                }
            }),
        )
        .route(
            "/",
            axum::routing::post(|| async {
                Json(json!({"access_token": "fixture-token", "expires_in": 300}))
            }),
        );
    let source = TdxBikeAvailabilitySource::with_cache_ttl(
        network(),
        spawn_tdx_fixture(app).await,
        Duration::from_millis(20),
    );
    source.snapshot().await.unwrap();
    source.snapshot().await.unwrap();
    assert_eq!(taipei_calls.load(Ordering::Relaxed), 1);
    assert_eq!(new_taipei_calls.load(Ordering::Relaxed), 1);
    tokio::time::sleep(Duration::from_millis(40)).await;
    source.snapshot().await.unwrap();
    assert_eq!(taipei_calls.load(Ordering::Relaxed), 2);
    assert_eq!(new_taipei_calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn concurrent_consumers_share_one_network_refresh() {
    let taipei_calls = Arc::new(AtomicUsize::new(0));
    let new_taipei_calls = Arc::new(AtomicUsize::new(0));
    let taipei_counter = taipei_calls.clone();
    let new_taipei_counter = new_taipei_calls.clone();
    let app = Router::new()
        .route(
            "/v2/Bike/Availability/City/Taipei",
            get(move || {
                let counter = taipei_counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Json(json!([availability(
                        "TPE-1",
                        1,
                        1,
                        1,
                        TimeDuration::seconds(10),
                    )]))
                }
            }),
        )
        .route(
            "/v2/Bike/Availability/City/NewTaipei",
            get(move || {
                let counter = new_taipei_counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Json(json!([]))
                }
            }),
        )
        .route(
            "/",
            axum::routing::post(|| async {
                Json(json!({"access_token": "fixture-token", "expires_in": 300}))
            }),
        );
    let source = Arc::new(TdxBikeAvailabilitySource::new(
        network(),
        spawn_tdx_fixture(app).await,
    ));
    let results = join_all((0..30).map(|_| {
        let source = source.clone();
        async move { source.snapshot().await }
    }))
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(taipei_calls.load(Ordering::Relaxed), 1);
    assert_eq!(new_taipei_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn malformed_stale_only_and_provider_failure_never_fabricate_availability() {
    let malformed = source_with_routes(
        json!([{
            "StationUID": "TPE-1",
            "ServiceStatus": 1,
            "AvailableRentBikes": "not-a-count",
            "AvailableReturnBikes": 1,
            "UpdateTime": timestamp(TimeDuration::seconds(10))
        }]),
        json!([]),
    )
    .await;
    assert_eq!(
        malformed.snapshot().await.unwrap_err(),
        BikeAvailabilityError::RejectedPayload
    );

    let stale_only = source_with_routes(
        json!([availability(
            "TPE-STALE",
            1,
            1,
            1,
            TimeDuration::seconds(121),
        )]),
        json!([]),
    )
    .await;
    assert_eq!(
        stale_only.snapshot().await.unwrap_err(),
        BikeAvailabilityError::RejectedPayload
    );

    let app = Router::new()
        .route(
            "/v2/Bike/Availability/City/Taipei",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/v2/Bike/Availability/City/NewTaipei",
            get(|| async { Json(json!([])) }),
        )
        .route(
            "/",
            axum::routing::post(|| async {
                Json(json!({"access_token": "fixture-token", "expires_in": 300}))
            }),
        );
    let failing = TdxBikeAvailabilitySource::new(network(), spawn_tdx_fixture(app).await);
    assert!(matches!(
        failing.snapshot().await,
        Err(BikeAvailabilityError::TemporarilyUnavailable)
    ));
}
