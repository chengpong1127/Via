use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use via_server::domain::bikeshare::{BikeAvailabilityError, BikeAvailabilitySource};

use crate::support::tdx::spawn_tdx_fixture;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{StatusCode, Uri},
    response::IntoResponse,
    routing::{any, get},
};
use futures_util::future::join_all;
use serde_json::json;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex;
use via_server::{
    domain::{
        bikeshare::{BikeShareNetwork, BikeStation, BikeStationAccess, BikeWalkingAccess},
        ids::{BikeStationId, BoardingPointId},
        location::GeoCoordinate,
    },
    providers::TdxBikeAvailabilitySource,
};

fn station(id: &str) -> BikeStation {
    BikeStation::new(
        BikeStationId::new(id),
        id,
        GeoCoordinate {
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
    TdxBikeAvailabilitySource::new(
        client,
        via_server::config::ViaConfig::default()
            .tdx
            .batch
            .bike_stations,
        via_server::config::ViaConfig::default().tdx.bikeshare,
    )
}

fn id(value: &str) -> BikeStationId {
    BikeStationId::new(value)
}

type AvailabilityRequests = Vec<(String, HashMap<String, String>)>;

#[derive(Clone)]
struct AvailabilityFixture {
    requests: Arc<Mutex<AvailabilityRequests>>,
    payloads: Arc<HashMap<String, (StatusCode, serde_json::Value)>>,
}

async fn availability_response(
    State(fixture): State<AvailabilityFixture>,
    uri: Uri,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let city = uri.path().rsplit('/').next().unwrap_or_default().to_owned();
    fixture.requests.lock().await.push((city.clone(), query));
    let (status, payload) = fixture
        .payloads
        .get(&city)
        .cloned()
        .unwrap_or((StatusCode::OK, json!([])));
    (status, Json(payload))
}

async fn source_for_cities(
    payloads: HashMap<String, (StatusCode, serde_json::Value)>,
    cache_ttl: Duration,
) -> (TdxBikeAvailabilitySource, AvailabilityFixture) {
    source_for_cities_with_settings(
        payloads,
        cache_ttl,
        via_server::config::ViaConfig::default().tdx,
    )
    .await
}

async fn source_for_cities_with_settings(
    payloads: HashMap<String, (StatusCode, serde_json::Value)>,
    cache_ttl: Duration,
    settings: via_server::config::TdxSettings,
) -> (TdxBikeAvailabilitySource, AvailabilityFixture) {
    let fixture = AvailabilityFixture {
        requests: Arc::new(Mutex::new(Vec::new())),
        payloads: Arc::new(payloads),
    };
    let app = Router::new()
        .route(
            "/v2/Bike/Availability/City/{city}",
            any(availability_response),
        )
        .route(
            "/",
            axum::routing::post(|| async {
                Json(json!({"access_token": "fixture-token", "expires_in": 300}))
            }),
        )
        .with_state(fixture.clone());
    let batch_size = settings.batch.bike_stations;
    let availability = via_server::tdx::BikeAvailabilitySettings {
        cache_ttl_seconds: cache_ttl.as_secs(),
        ..settings.bikeshare.clone()
    };
    let source = TdxBikeAvailabilitySource::new(
        crate::support::tdx::spawn_tdx_fixture_with_settings(app, settings).await,
        batch_size,
        availability,
    );
    (source, fixture)
}

fn station_uids(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|index| format!("{prefix}{index}")).collect()
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

    let snapshot = source
        .snapshot(&[id("tdx:Taipei:TPE-1"), id("tdx:NewTaipei:NWT-1")])
        .await
        .unwrap();
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
    let snapshot = source
        .snapshot(&[
            id("tdx:Taipei:TPE-1"),
            id("tdx:Taipei:TPE-DISABLED"),
            id("tdx:Taipei:TPE-STALE"),
            id("tdx:NewTaipei:NWT-1"),
        ])
        .await
        .unwrap();
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
async fn provider_clock_skew_within_the_allowance_is_tolerated_but_excess_is_rejected() {
    let tolerated = source_with_routes(
        json!([availability("TPE-1", 1, 2, 1, -TimeDuration::seconds(60),)]),
        json!([]),
    )
    .await;
    let snapshot = tolerated.snapshot(&[id("tdx:Taipei:TPE-1")]).await.unwrap();
    assert_eq!(
        snapshot
            .station(&BikeStationId::new("tdx:Taipei:TPE-1"))
            .unwrap()
            .available_bikes,
        2
    );
    assert_eq!(
        network()
            .availability_view(&snapshot)
            .usable_pickups(&BoardingPointId::new("pickup"))
            .len(),
        1
    );

    let excessive = source_with_routes(
        json!([availability("TPE-1", 1, 2, 1, -TimeDuration::seconds(301),)]),
        json!([]),
    )
    .await;
    assert_eq!(
        excessive
            .snapshot(&[id("tdx:Taipei:TPE-1")])
            .await
            .unwrap_err(),
        BikeAvailabilityError::RejectedPayload
    );
}

#[tokio::test]
async fn station_availability_is_cached_and_expiry_refreshes_requested_city() {
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
    let source = TdxBikeAvailabilitySource::new(
        spawn_tdx_fixture(app).await,
        via_server::config::ViaConfig::default()
            .tdx
            .batch
            .bike_stations,
        via_server::tdx::BikeAvailabilitySettings {
            cache_ttl_seconds: (Duration::from_secs(1)).as_secs(),
            ..via_server::config::ViaConfig::default().tdx.bikeshare
        },
    );
    let requested = [id("tdx:Taipei:TPE-1")];
    source.snapshot(&requested).await.unwrap();
    source.snapshot(&requested).await.unwrap();
    assert_eq!(taipei_calls.load(Ordering::Relaxed), 1);
    assert_eq!(new_taipei_calls.load(Ordering::Relaxed), 0);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    source.snapshot(&requested).await.unwrap();
    assert_eq!(taipei_calls.load(Ordering::Relaxed), 2);
    assert_eq!(new_taipei_calls.load(Ordering::Relaxed), 0);
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
        spawn_tdx_fixture(app).await,
        via_server::config::ViaConfig::default()
            .tdx
            .batch
            .bike_stations,
        via_server::config::ViaConfig::default().tdx.bikeshare,
    ));
    let requested = [id("tdx:Taipei:TPE-1")];
    let results = join_all((0..30).map(|_| {
        let source = source.clone();
        let requested = requested.clone();
        async move { source.snapshot(&requested).await }
    }))
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(taipei_calls.load(Ordering::Relaxed), 1);
    assert_eq!(new_taipei_calls.load(Ordering::Relaxed), 0);
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
        malformed
            .snapshot(&[id("tdx:Taipei:TPE-1")])
            .await
            .unwrap_err(),
        BikeAvailabilityError::RejectedPayload
    );

    let stale_only = source_with_routes(
        json!([availability(
            "TPE-STALE",
            1,
            1,
            1,
            TimeDuration::seconds(181),
        )]),
        json!([]),
    )
    .await;
    assert_eq!(
        stale_only
            .snapshot(&[id("tdx:Taipei:TPE-STALE")])
            .await
            .unwrap_err(),
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
    let failing = TdxBikeAvailabilitySource::new(
        spawn_tdx_fixture(app).await,
        via_server::config::ViaConfig::default()
            .tdx
            .batch
            .bike_stations,
        via_server::config::ViaConfig::default().tdx.bikeshare,
    );

    assert!(matches!(
        failing.snapshot(&[id("tdx:Taipei:TPE-1")]).await,
        Err(BikeAvailabilityError::TemporarilyUnavailable)
    ));
}

#[tokio::test]
async fn requests_only_missing_station_ids_after_a_partial_cache_hit() {
    let payloads = json!([
        availability("A", 1, 1, 1, TimeDuration::seconds(10)),
        availability("B", 1, 2, 2, TimeDuration::seconds(10)),
        availability("C", 1, 3, 3, TimeDuration::seconds(10)),
    ]);
    let (source, fixture) = source_for_cities(
        HashMap::from([("Taipei".to_owned(), (StatusCode::OK, payloads))]),
        Duration::from_secs(30),
    )
    .await;

    let cached = source.snapshot(&[id("tdx:Taipei:A")]).await.unwrap();
    let snapshot = source
        .snapshot(&[id("tdx:Taipei:A"), id("tdx:Taipei:B"), id("tdx:Taipei:C")])
        .await
        .unwrap();
    assert_eq!(snapshot.stations.len(), 3);
    assert_eq!(
        snapshot.station(&id("tdx:Taipei:A")),
        cached.station(&id("tdx:Taipei:A"))
    );

    let requests = fixture.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, "Taipei");
    assert_eq!(requests[0].1.get("$top").map(String::as_str), Some("1"));
    assert_eq!(
        requests[1].1.get("$filter").map(String::as_str),
        Some("StationUID in ('B','C')")
    );
    assert_eq!(requests[1].1.get("$top").map(String::as_str), Some("2"));
}

#[tokio::test]
async fn groups_requested_stations_by_city_and_escapes_uid_filter_literals() {
    let (source, fixture) = source_for_cities(
        HashMap::from([
            (
                "Taipei".to_owned(),
                (
                    StatusCode::OK,
                    json!([availability("TPE-1", 1, 1, 1, TimeDuration::seconds(10))]),
                ),
            ),
            (
                "Taichung".to_owned(),
                (
                    StatusCode::OK,
                    json!([
                        availability("TC-1", 1, 2, 2, TimeDuration::seconds(10)),
                        availability("A'B", 1, 3, 3, TimeDuration::seconds(10)),
                    ]),
                ),
            ),
        ]),
        Duration::from_secs(30),
    )
    .await;
    let snapshot = source
        .snapshot(&[
            id("tdx:Taipei:TPE-1"),
            id("tdx:Taichung:TC-1"),
            id("tdx:Taichung:A'B"),
        ])
        .await
        .unwrap();
    assert_eq!(snapshot.stations.len(), 3);

    let mut requests = fixture.requests.lock().await.clone();
    requests.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, "Taichung");
    assert_eq!(
        requests[0].1.get("$filter").map(String::as_str),
        Some("StationUID in ('A''B','TC-1')")
    );
    assert_eq!(requests[0].1.get("$top").map(String::as_str), Some("2"));
    assert_eq!(requests[1].0, "Taipei");
    assert_eq!(
        requests[1].1.get("$filter").map(String::as_str),
        Some("StationUID in ('TPE-1')")
    );
}

#[tokio::test]
async fn availability_requests_are_batched_at_three_hundred_stations() {
    for count in [300usize, 301] {
        let uids = station_uids("UID-", count);
        let response = uids
            .iter()
            .map(|uid| availability(uid, 1, 1, 1, TimeDuration::seconds(10)))
            .collect::<Vec<_>>();
        let (source, fixture) = source_for_cities(
            HashMap::from([("Taipei".to_owned(), (StatusCode::OK, json!(response)))]),
            Duration::from_secs(30),
        )
        .await;
        let requested = uids
            .iter()
            .map(|uid| id(&format!("tdx:Taipei:{uid}")))
            .collect::<Vec<_>>();

        let snapshot = source.snapshot(&requested).await.unwrap();
        assert_eq!(snapshot.stations.len(), count);
        let requests = fixture.requests.lock().await;
        assert_eq!(requests.len(), count.div_ceil(300));
        let tops = requests
            .iter()
            .map(|(_, query)| query.get("$top").unwrap().parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(tops.iter().sum::<usize>(), count);
        assert!(tops.contains(&300));
        assert_eq!(tops.contains(&1), count == 301);
    }
}

#[tokio::test]
async fn empty_and_invalid_requests_do_not_call_tdx() {
    let (source, fixture) = source_for_cities(HashMap::new(), Duration::from_secs(30)).await;
    assert!(source.snapshot(&[]).await.unwrap().stations.is_empty());
    assert!(
        source
            .snapshot(&[id("tdx:Unknown:UID"), id("tdx:Taipei:invalid:uid")])
            .await
            .unwrap()
            .stations
            .is_empty()
    );
    assert!(fixture.requests.lock().await.is_empty());
}

#[tokio::test]
async fn stale_cached_observation_is_refetched_before_cache_ttl() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new()
        .route(
            "/v2/Bike/Availability/City/Taipei",
            get(move || {
                let counter = counter.clone();
                async move {
                    let call = counter.fetch_add(1, Ordering::Relaxed);
                    let age = if call == 0 {
                        TimeDuration::seconds(179)
                    } else {
                        TimeDuration::seconds(10)
                    };
                    Json(json!([availability("TPE-1", 1, call as i32 + 1, 1, age)]))
                }
            }),
        )
        .route(
            "/",
            axum::routing::post(|| async {
                Json(json!({"access_token": "fixture-token", "expires_in": 300}))
            }),
        );
    let source = TdxBikeAvailabilitySource::new(
        spawn_tdx_fixture(app).await,
        via_server::config::ViaConfig::default()
            .tdx
            .batch
            .bike_stations,
        via_server::tdx::BikeAvailabilitySettings {
            cache_ttl_seconds: (Duration::from_secs(30)).as_secs(),
            ..via_server::config::ViaConfig::default().tdx.bikeshare
        },
    );
    let requested = [id("tdx:Taipei:TPE-1")];
    source.snapshot(&requested).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let refreshed = source.snapshot(&requested).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        refreshed
            .station(&id("tdx:Taipei:TPE-1"))
            .unwrap()
            .available_bikes,
        2
    );
}

#[tokio::test]
async fn partial_city_failure_keeps_successful_and_cached_station_data() {
    let (source, fixture) = source_for_cities(
        HashMap::from([
            (
                "Taipei".to_owned(),
                (
                    StatusCode::OK,
                    json!([availability("TPE-1", 1, 4, 2, TimeDuration::seconds(10))]),
                ),
            ),
            (
                "Taichung".to_owned(),
                (StatusCode::INTERNAL_SERVER_ERROR, json!({})),
            ),
        ]),
        Duration::from_secs(30),
    )
    .await;
    let requested = [id("tdx:Taipei:TPE-1"), id("tdx:Taichung:TC-1")];
    let first = source.snapshot(&requested).await.unwrap();
    assert_eq!(first.stations.len(), 1);
    assert_eq!(
        first
            .station(&id("tdx:Taipei:TPE-1"))
            .unwrap()
            .available_bikes,
        4
    );
    let second = source.snapshot(&requested).await.unwrap();
    assert_eq!(second.stations.len(), 1);
    let requests = fixture.requests.lock().await;
    assert_eq!(
        requests.iter().filter(|(city, _)| city == "Taipei").count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(city, _)| city == "Taichung")
            .count(),
        2
    );
}

#[tokio::test]
async fn changed_toml_batch_size_applies_to_every_supported_bike_city() {
    let config = via_server::config::ViaConfig::from_toml(
        &include_str!("../../config/via.toml").replace("bike_stations = 300", "bike_stations = 3"),
    )
    .unwrap();
    for (city, _) in via_server::tdx::BIKE_CITIES {
        let uids = station_uids("station-", 7);
        let payload = uids
            .iter()
            .map(|uid| availability(uid, 1, 1, 1, TimeDuration::seconds(10)))
            .collect::<Vec<_>>();
        let (source, fixture) = source_for_cities_with_settings(
            HashMap::from([(city.to_string(), (StatusCode::OK, json!(payload)))]),
            Duration::from_secs(30),
            config.tdx.clone(),
        )
        .await;
        let ids = uids
            .iter()
            .map(|uid| id(&format!("tdx:{city}:{uid}")))
            .collect::<Vec<_>>();
        let snapshot = source.snapshot(&ids).await.unwrap();
        assert_eq!(snapshot.stations.len(), 7, "{city}");
        let requests = fixture.requests.lock().await;
        assert_eq!(requests.len(), 3, "{city}");
        let tops = requests
            .iter()
            .map(|(_, query)| query["$top"].parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(tops, vec![3, 3, 1], "{city}");
    }
}
