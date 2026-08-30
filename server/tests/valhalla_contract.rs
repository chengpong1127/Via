use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

mod support;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use time::macros::datetime;
use via_server::street::{
    Coordinate, DetailedPathQuery, GraphRevision, ManeuverKind, MatrixOrientation, MatrixQuery,
    OperationalEvent, OperationalEventObserver, ProviderError, RoutingConstraints, RoutingTime,
    RoutingTimeKind, StreetRoutingSession, StreetTravelMode, ValhallaConfig, ValhallaProvider,
};

use support::{
    ProviderContractFixture, assert_cancellation_follow_up_succeeds, assert_malformed_response,
    assert_provider_contract, assert_rate_limited, assert_revision_mismatch, assert_timeout,
    assert_unreachable,
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

async fn locate(State(state): State<FixtureState>, Json(request): Json<Value>) -> Json<Value> {
    state.requests.lock().unwrap().push(request);
    Json(json!([
        {"edges": [{"correlated_lat": 25.04770, "correlated_lon": 121.51710}]}
    ]))
}

async fn matrix(State(state): State<FixtureState>, Json(request): Json<Value>) -> Json<Value> {
    let source_count = request["sources"].as_array().unwrap().len();
    let target_count = request["targets"].as_array().unwrap().len();
    state.requests.lock().unwrap().push(request);
    let reachable = json!({
        "time": 620.4,
        "distance": 3.9,
        "begin_lat": 25.04770,
        "begin_lon": 121.51710,
        "end_lat": 25.03390,
        "end_lon": 121.56440
    });
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
        .route("/locate", post(locate))
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
        graph_revision: GraphRevision::new("taipei-2026-08-30"),
        request_timeout: Duration::from_secs(1),
        max_concurrency: 2,
        max_sources: 4,
        max_targets: 4,
        max_correlation_locations: 4,
        max_response_bytes: 64 * 1024,
        expected_tileset_last_modified: 1_788_019_200,
    }
}

#[tokio::test]
async fn valhalla_adapter_normalizes_all_provider_contract_capabilities() {
    let (base_url, state) = spawn_fixture().await;
    let provider = Arc::new(ValhallaProvider::new(config(base_url)).unwrap());
    let session = StreetRoutingSession::start(provider).await.unwrap();
    let origin = coordinate(25.04776, 121.51706);
    let destination = coordinate(25.03396, 121.56447);
    let unreachable = coordinate(25.03344, 121.53409);

    let path = assert_provider_contract(
        &session,
        &ProviderContractFixture {
            origin,
            destination,
            unreachable,
            alternate_origin: unreachable,
            duration: via_server::street::Seconds(620),
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

    session
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: RoutingConstraints {
                time: Some(RoutingTime {
                    kind: RoutingTimeKind::DepartAt,
                    local_date_time: datetime!(2026-08-30 08:15),
                }),
                max_distance: Some(via_server::street::Metres(5_000)),
            },
            travel_mode: StreetTravelMode::YouBike,
            sources: vec![origin],
            targets: vec![destination, unreachable],
        })
        .await
        .unwrap();

    let requests = state.requests.lock().unwrap();
    assert_eq!(requests[1]["costing"], "bicycle");
    assert_eq!(requests[1]["verbose"], true);
    assert_eq!(requests[2]["costing"], "pedestrian");
    assert_eq!(requests[3]["shape_format"], "polyline6");
    assert_eq!(requests[4]["date_time"]["type"], 1);
    assert_eq!(requests[4]["date_time"]["value"], "2026-08-30T08:15");
    assert_eq!(requests[4]["expansion_max_distance"], 5_000);
}

#[derive(Default)]
struct RecordingObserver(Mutex<Vec<OperationalEvent>>);

impl OperationalEventObserver for RecordingObserver {
    fn observe(&self, event: OperationalEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn operational_events_contains_counts_but_never_coordinates_or_paths() {
    let (base_url, _) = spawn_fixture().await;
    let observer = Arc::new(RecordingObserver::default());
    let provider =
        Arc::new(ValhallaProvider::with_observer(config(base_url), observer.clone()).unwrap());
    let session = StreetRoutingSession::start(provider).await.unwrap();
    session
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

    let events = observer.0.lock().unwrap();
    let serialized = serde_json::to_string(&events.as_slice()).unwrap();
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
    let provider = Arc::new(
        ValhallaProvider::new(config(
            url::Url::parse(&format!("http://{address}/")).unwrap(),
        ))
        .unwrap(),
    );
    let session = StreetRoutingSession::start(provider).await.unwrap();

    let error = session
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
async fn valhalla_unreachable_error_codes_are_translated_without_leaking_payloads() {
    let app = Router::new().route("/status", get(status)).route(
        "/route",
        post(|| async {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error_code": 171,
                    "error": "No suitable edges near location",
                    "status_code": 400
                })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = Arc::new(
        ValhallaProvider::new(config(
            url::Url::parse(&format!("http://{address}/")).unwrap(),
        ))
        .unwrap(),
    );
    let session = StreetRoutingSession::start(provider).await.unwrap();

    let error = session
        .detailed_path(DetailedPathQuery {
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
    let app = Router::new()
        .route("/status", get(status))
        .route("/route", post(|| async { Json(json!({"trip": {}})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = Arc::new(
        ValhallaProvider::new(config(
            url::Url::parse(&format!("http://{address}/")).unwrap(),
        ))
        .unwrap(),
    );
    let session = StreetRoutingSession::start(provider).await.unwrap();

    let error = session
        .detailed_path(DetailedPathQuery {
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
async fn startup_rejects_a_tileset_that_does_not_match_the_configured_revision() {
    let (base_url, _) = spawn_fixture().await;
    let mut mismatched = config(base_url);
    mismatched.expected_tileset_last_modified = 1;
    let provider = Arc::new(ValhallaProvider::new(mismatched).unwrap());

    let error = match StreetRoutingSession::start(provider).await {
        Ok(_) => panic!("mismatched graph must fail startup"),
        Err(error) => error,
    };
    assert_revision_mismatch(error);
}

#[tokio::test]
async fn valhalla_adapter_enforces_matrix_and_response_size_bounds() {
    let (base_url, _) = spawn_fixture().await;
    let mut bounded = config(base_url.clone());
    bounded.max_sources = 1;
    let provider = Arc::new(ValhallaProvider::new(bounded).unwrap());
    let session = StreetRoutingSession::start(provider).await.unwrap();
    let matrix_error = session
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
    assert!(matches!(matrix_error, ProviderError::InvalidRequest { .. }));

    let mut tiny_response = config(base_url);
    tiny_response.max_response_bytes = 8;
    let provider = Arc::new(ValhallaProvider::new(tiny_response).unwrap());
    let startup_error = match StreetRoutingSession::start(provider).await {
        Ok(_) => panic!("oversized health response must fail startup"),
        Err(error) => error,
    };
    assert_eq!(
        startup_error,
        ProviderError::ResponseTooLarge { limit_bytes: 8 }
    );
}

#[tokio::test]
async fn operation_deadline_includes_waiting_for_the_concurrency_permit() {
    async fn slow_matrix() -> Json<Value> {
        tokio::time::sleep(Duration::from_millis(250)).await;
        Json(json!({"sources_to_targets": [[{"time": 1, "distance": 0.001}]]}))
    }

    let app = Router::new()
        .route("/status", get(status))
        .route("/sources_to_targets", post(slow_matrix));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut deadline_config = config(url::Url::parse(&format!("http://{address}/")).unwrap());
    deadline_config.max_concurrency = 1;
    deadline_config.request_timeout = Duration::from_millis(40);
    let provider = Arc::new(ValhallaProvider::new(deadline_config).unwrap());
    let session = StreetRoutingSession::start(provider).await.unwrap();
    let query = MatrixQuery {
        orientation: MatrixOrientation::OneToMany,
        constraints: Default::default(),
        travel_mode: StreetTravelMode::Walking,
        sources: vec![coordinate(25.0, 121.5)],
        targets: vec![coordinate(25.1, 121.6)],
    };

    let first_session = session.clone();
    let first_query = query.clone();
    let first = tokio::spawn(async move { first_session.matrix(first_query).await });
    tokio::time::sleep(Duration::from_millis(5)).await;
    let started = std::time::Instant::now();
    let second_error = session.matrix(query).await.unwrap_err();

    assert_timeout(second_error);
    assert!(started.elapsed() < Duration::from_millis(80));
    assert_timeout(first.await.unwrap().unwrap_err());
}

#[tokio::test]
async fn cancelling_an_operation_releases_its_concurrency_permit() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn cancellable_matrix(State(calls): State<Arc<AtomicUsize>>) -> Json<Value> {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Json(json!({
            "sources_to_targets": [[{"time": 1, "distance": 0.001}]]
        }))
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/status", get(status))
        .route("/sources_to_targets", post(cancellable_matrix))
        .with_state(calls);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cancellation_config = config(url::Url::parse(&format!("http://{address}/")).unwrap());
    cancellation_config.max_concurrency = 1;
    cancellation_config.request_timeout = Duration::from_secs(2);
    let session = StreetRoutingSession::start(Arc::new(
        ValhallaProvider::new(cancellation_config).unwrap(),
    ))
    .await
    .unwrap();
    let query = MatrixQuery {
        orientation: MatrixOrientation::OneToMany,
        constraints: Default::default(),
        travel_mode: StreetTravelMode::Walking,
        sources: vec![coordinate(25.0, 121.5)],
        targets: vec![coordinate(25.1, 121.6)],
    };

    let cancelled_session = session.clone();
    let cancelled_query = query.clone();
    let cancelled = tokio::spawn(async move { cancelled_session.matrix(cancelled_query).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancelled.abort();

    let follow_up = session.matrix(query).await;
    assert_cancellation_follow_up_succeeds(follow_up.clone());
    let result = follow_up.unwrap();
    assert!(matches!(
        result.cells[0][0].reachability,
        via_server::street::MatrixReachability::Reachable { .. }
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
