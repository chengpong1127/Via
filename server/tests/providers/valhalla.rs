use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::support::{
    ProviderContractFixture, assert_cancellation_follow_up_succeeds, assert_malformed_response,
    assert_provider_contract, assert_rate_limited, assert_timeout, assert_unreachable,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use time::macros::datetime;
use via_server::{
    health::DependencyHealth,
    providers::{OperationalEvent, OperationalEventObserver, ValhallaConfig, ValhallaProvider},
    street::{
        Coordinate, ManeuverKind, MatrixCell, MatrixOrientation, MatrixQuery, RouteQuery,
        RoutingConstraints, RoutingTime, RoutingTimeKind, Seconds, StreetRouter,
        StreetRoutingError, StreetTravelMode,
    },
};

#[derive(Clone, Default)]
struct FixtureState {
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn status() -> Json<Value> {
    Json(json!({
        "version": "3.8.3",
        "tileset_last_modified": 1_788_019_200_u64,
        "has_tiles": true
    }))
}

async fn matrix(State(state): State<FixtureState>, Json(request): Json<Value>) -> Json<Value> {
    let source_count = request["sources"].as_array().unwrap().len();
    let target_count = request["targets"].as_array().unwrap().len();
    state.requests.lock().unwrap().push(request);
    let reachable = json!({"time": 620.4, "distance": 3.9});
    let cells = if source_count == 2 && target_count == 1 {
        json!([[reachable.clone()], [reachable]])
    } else {
        json!([[reachable, {"time": null, "distance": null}]])
    };
    Json(json!({"sources_to_targets": cells}))
}

async fn route(State(state): State<FixtureState>, Json(request): Json<Value>) -> Json<Value> {
    state.requests.lock().unwrap().push(request);
    Json(json!({
        "trip": {
            "summary": {"time": 620.4, "length": 3.9},
            "legs": [{
                "shape": encode_polyline6(&[(25.04776, 121.51706), (25.03396, 121.56447)]),
                "maneuvers": [
                    {"type": 1, "begin_shape_index": 0, "end_shape_index": 0},
                    {"type": 10, "begin_shape_index": 0, "end_shape_index": 1, "street_names": ["信義路"]},
                    {"type": 12, "begin_shape_index": 1, "end_shape_index": 1},
                    {"type": 4, "begin_shape_index": 1, "end_shape_index": 1}
                ]
            }]
        }
    }))
}

async fn spawn_fixture() -> (url::Url, FixtureState) {
    let state = FixtureState::default();
    let app = Router::new()
        .route("/status", get(status))
        .route("/sources_to_targets", post(matrix))
        .route("/route", post(route))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        url::Url::parse(&format!("http://{address}/")).unwrap(),
        state,
    )
}

fn coordinate(latitude: f64, longitude: f64) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

fn config(base_url: url::Url) -> ValhallaConfig {
    ValhallaConfig {
        base_url,
        request_timeout: Duration::from_secs(1),
        max_concurrency: 2,
        max_sources: 4,
        max_targets: 4,
        max_response_bytes: 64 * 1024,
    }
}

#[tokio::test]
async fn valhalla_adapter_normalizes_the_provider_contract() {
    let (base_url, state) = spawn_fixture().await;
    let provider = Arc::new(ValhallaProvider::new(config(base_url)).unwrap());
    assert!(provider.healthy().await);
    let origin = coordinate(25.04776, 121.51706);
    let destination = coordinate(25.03396, 121.56447);
    let unreachable = coordinate(25.03344, 121.53409);

    let path = assert_provider_contract(
        provider.as_ref(),
        &ProviderContractFixture {
            origin,
            destination,
            unreachable,
            alternate_origin: unreachable,
            duration: Seconds(620),
            distance: via_server::street::Metres(3_900),
            geometry: vec![origin, destination],
            maneuver_kinds: vec![
                ManeuverKind::Depart,
                ManeuverKind::TurnRight,
                ManeuverKind::UTurn,
                ManeuverKind::Arrive,
            ],
        },
    )
    .await;
    assert_eq!(path.maneuvers[1].kind, ManeuverKind::TurnRight);
    assert_eq!(path.maneuvers[2].kind, ManeuverKind::UTurn);
    assert_eq!(path.maneuvers[1].street_names, vec!["信義路"]);
    assert_eq!(path.attribution[0].text, "© OpenStreetMap contributors");

    provider
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: RoutingConstraints {
                time: Some(RoutingTime {
                    kind: RoutingTimeKind::DepartAt,
                    local_date_time: datetime!(2026-08-30 08:15),
                }),
                max_distance: Some(via_server::street::Metres(5_000)),
            },
            travel_mode: StreetTravelMode::Bicycle,
            sources: vec![origin],
            targets: vec![destination, unreachable],
        })
        .await
        .unwrap();

    let requests = state.requests.lock().unwrap();
    assert_eq!(requests[0]["costing"], "bicycle");
    assert_eq!(requests[0]["verbose"], true);
    assert_eq!(requests[1]["costing"], "pedestrian");
    assert_eq!(requests[2]["shape_format"], "polyline6");
    assert_eq!(requests[3]["date_time"]["type"], 1);
    assert_eq!(requests[3]["date_time"]["value"], "2026-08-30T08:15");
    assert_eq!(requests[3]["expansion_max_distance"], 5_000);
}

#[derive(Default)]
struct RecordingObserver(Mutex<Vec<OperationalEvent>>);

impl OperationalEventObserver for RecordingObserver {
    fn observe(&self, event: OperationalEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn operational_events_contain_counts_but_never_coordinates_or_paths() {
    let (base_url, _) = spawn_fixture().await;
    let observer = Arc::new(RecordingObserver::default());
    let provider =
        Arc::new(ValhallaProvider::with_observer(config(base_url), observer.clone()).unwrap());
    provider
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![coordinate(25.04776, 121.51706)],
            targets: vec![
                coordinate(25.03396, 121.56447),
                coordinate(25.03344, 121.53409),
            ],
        })
        .await
        .unwrap();

    let serialized = serde_json::to_string(&observer.0.lock().unwrap().as_slice()).unwrap();
    assert!(serialized.contains("source_count"));
    assert!(!serialized.contains("25.04776"));
    assert!(!serialized.contains("121.51706"));
    assert!(!serialized.contains("geometry"));
}

#[tokio::test]
async fn valhalla_rate_limits_are_translated_to_a_via_owned_error() {
    let app = Router::new().route("/status", get(status)).route(
        "/sources_to_targets",
        post(|| async { StatusCode::TOO_MANY_REQUESTS.into_response() }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = ValhallaProvider::new(config(
        url::Url::parse(&format!("http://{address}/")).unwrap(),
    ))
    .unwrap();
    let error = provider
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![coordinate(25.0, 121.5)],
            targets: vec![coordinate(25.1, 121.6)],
        })
        .await
        .unwrap_err();
    assert_rate_limited(error);
}

#[tokio::test]
async fn valhalla_unreachable_errors_do_not_leak_payloads() {
    let app = Router::new().route(
        "/route",
        post(|| async {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error_code": 171, "error": "No suitable edges near location"})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = ValhallaProvider::new(config(
        url::Url::parse(&format!("http://{address}/")).unwrap(),
    ))
    .unwrap();
    let error = provider
        .route(RouteQuery {
            travel_mode: StreetTravelMode::Walking,
            constraints: Default::default(),
            origin: coordinate(25.0, 121.5),
            destination: coordinate(25.1, 121.6),
        })
        .await
        .unwrap_err();
    assert_unreachable(error.clone());
    assert!(!error.to_string().contains("No suitable edges"));
}

#[tokio::test]
async fn malformed_valhalla_responses_use_the_shared_contract_error() {
    let app = Router::new().route("/route", post(|| async { Json(json!({"trip": {}})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = ValhallaProvider::new(config(
        url::Url::parse(&format!("http://{address}/")).unwrap(),
    ))
    .unwrap();
    let error = provider
        .route(RouteQuery {
            travel_mode: StreetTravelMode::Walking,
            constraints: Default::default(),
            origin: coordinate(25.0, 121.5),
            destination: coordinate(25.1, 121.6),
        })
        .await
        .unwrap_err();
    assert_malformed_response(error);
}

#[tokio::test]
async fn health_does_not_compare_valhalla_artifact_identity() {
    let (base_url, _) = spawn_fixture().await;
    let provider = ValhallaProvider::new(config(base_url)).unwrap();
    assert!(provider.healthy().await);
}

#[tokio::test]
async fn health_rejects_a_status_response_without_a_healthy_signal() {
    let app = Router::new().route(
        "/status",
        get(|| async { Json(json!({"version": "3.9.0"})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = ValhallaProvider::new(config(
        url::Url::parse(&format!("http://{address}/")).unwrap(),
    ))
    .unwrap();
    assert!(!provider.healthy().await);
}

#[tokio::test]
async fn valhalla_adapter_enforces_its_own_matrix_and_response_bounds() {
    let (base_url, _) = spawn_fixture().await;
    let mut bounded = config(base_url.clone());
    bounded.max_sources = 1;
    let provider = ValhallaProvider::new(bounded).unwrap();
    let matrix_error = provider
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::ManyToOne,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![
                coordinate(25.04776, 121.51706),
                coordinate(25.03344, 121.53409),
            ],
            targets: vec![coordinate(25.03396, 121.56447)],
        })
        .await
        .unwrap_err();
    assert!(matches!(
        matrix_error,
        StreetRoutingError::InvalidRequest { .. }
    ));

    let mut tiny_response = config(base_url);
    tiny_response.max_response_bytes = 8;
    let provider = ValhallaProvider::new(tiny_response).unwrap();
    assert!(!provider.healthy().await);
}

#[tokio::test]
async fn operation_deadline_includes_waiting_for_the_concurrency_permit() {
    async fn slow_matrix() -> Json<Value> {
        tokio::time::sleep(Duration::from_millis(250)).await;
        Json(json!({"sources_to_targets": [[{"time": 1, "distance": 0.001}]]}))
    }

    let app = Router::new().route("/sources_to_targets", post(slow_matrix));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut deadline_config = config(url::Url::parse(&format!("http://{address}/")).unwrap());
    deadline_config.max_concurrency = 1;
    deadline_config.request_timeout = Duration::from_millis(40);
    let provider = Arc::new(ValhallaProvider::new(deadline_config).unwrap());
    let query = MatrixQuery {
        orientation: MatrixOrientation::OneToMany,
        constraints: Default::default(),
        travel_mode: StreetTravelMode::Walking,
        sources: vec![coordinate(25.0, 121.5)],
        targets: vec![coordinate(25.1, 121.6)],
    };

    let first_provider = provider.clone();
    let first_query = query.clone();
    let first = tokio::spawn(async move { first_provider.matrix(first_query).await });
    tokio::time::sleep(Duration::from_millis(5)).await;
    let started = std::time::Instant::now();
    let second_error = provider.matrix(query).await.unwrap_err();
    assert_timeout(second_error);
    assert!(started.elapsed() < Duration::from_millis(80));
    assert_timeout(first.await.unwrap().unwrap_err());
}

#[tokio::test]
async fn cancelling_an_operation_releases_its_concurrency_permit() {
    async fn cancellable_matrix(State(calls): State<Arc<AtomicUsize>>) -> Json<Value> {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Json(json!({"sources_to_targets": [[{"time": 1, "distance": 0.001}]]}))
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/sources_to_targets", post(cancellable_matrix))
        .with_state(calls);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cancellation_config = config(url::Url::parse(&format!("http://{address}/")).unwrap());
    cancellation_config.max_concurrency = 1;
    cancellation_config.request_timeout = Duration::from_secs(2);
    let provider = Arc::new(ValhallaProvider::new(cancellation_config).unwrap());
    let query = MatrixQuery {
        orientation: MatrixOrientation::OneToMany,
        constraints: Default::default(),
        travel_mode: StreetTravelMode::Walking,
        sources: vec![coordinate(25.0, 121.5)],
        targets: vec![coordinate(25.1, 121.6)],
    };

    let cancelled_provider = provider.clone();
    let cancelled_query = query.clone();
    let cancelled = tokio::spawn(async move { cancelled_provider.matrix(cancelled_query).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancelled.abort();

    let follow_up = provider.matrix(query).await;
    assert_cancellation_follow_up_succeeds(follow_up.clone());
    assert!(matches!(
        follow_up.unwrap().cells[0][0],
        MatrixCell::Reachable { .. }
    ));
}

fn encode_polyline6(points: &[(f64, f64)]) -> String {
    let mut encoded = String::new();
    let mut previous_latitude = 0_i64;
    let mut previous_longitude = 0_i64;
    for &(latitude, longitude) in points {
        let latitude = (latitude * 1_000_000.0).round() as i64;
        let longitude = (longitude * 1_000_000.0).round() as i64;
        encode_component(latitude - previous_latitude, &mut encoded);
        encode_component(longitude - previous_longitude, &mut encoded);
        previous_latitude = latitude;
        previous_longitude = longitude;
    }
    encoded
}

fn encode_component(value: i64, output: &mut String) {
    let mut value = if value < 0 {
        (!(value << 1)) as u64
    } else {
        (value << 1) as u64
    };
    while value >= 0x20 {
        output.push(char::from_u32(((0x20 | (value & 0x1f)) + 63) as u32).unwrap());
        value >>= 5;
    }
    output.push(char::from_u32((value + 63) as u32).unwrap());
}
