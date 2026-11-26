use std::{sync::Arc, time::Duration};

use crate::support::{
    build_test_router_with_network,
    log_capture::{CapturedLogs, capture_trace_logs},
    tdx::spawn_tdx_fixture,
};
use axum::{
    Json, Router,
    response::IntoResponse,
    routing::{get, post},
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use via_server::{
    domain::{
        ids::{BoardingPointId, TransitPatternId, TransitRouteId},
        location::GeoCoordinate,
        transit::network::{
            SegmentProfile, SegmentTimeSource, SegmentTravelTime, TransitMode, TransitNetwork,
            TransitPattern, TransitRoute, TransitStop,
        },
    },
    street::FakeStreetRouter,
};

const SECRET_LATITUDES: [&str; 6] = [
    "25.03817", "25.04291", "25.03911", "25.038", "25.043", "25.039",
];
const SECRET_LONGITUDES: [&str; 6] = [
    "121.56349",
    "121.51872",
    "121.53407",
    "121.563",
    "121.519",
    "121.534",
];

fn stop(id: &str, name: &str, latitude: f64, longitude: f64) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        name,
        GeoCoordinate {
            latitude,
            longitude,
        },
    )
}

fn network() -> TransitNetwork {
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![
            stop("origin-stop", "Origin Stop", 25.03819, 121.56351),
            stop("destination-stop", "Destination Stop", 25.04293, 121.51874),
        ],
        vec![TransitPattern::new(
            TransitPatternId::new("bus-1"),
            TransitRouteId::new("bus:1"),
            None,
            "Terminal",
            vec![
                BoardingPointId::new("origin-stop"),
                BoardingPointId::new("destination-stop"),
            ],
            SegmentProfile {
                segments: vec![SegmentTravelTime {
                    seconds: 600,
                    source: SegmentTimeSource::Derived,
                }],
            },
        )],
        Vec::new(),
    )
    .with_routes(vec![TransitRoute {
        id: TransitRouteId::new("bus:1"),
        mode: TransitMode::Bus,
        label: "1".into(),
    }])
    .with_revisions("static-1", "schedule-1")
}

async fn send(request: Request<Body>) -> StatusCode {
    let app =
        build_test_router_with_network(Arc::new(FakeStreetRouter::builder().build()), network())
            .await
            .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    response.into_body().collect().await.unwrap();
    status
}

fn assert_no_coordinates(logs: &CapturedLogs) {
    let text = logs.text();
    assert!(!text.is_empty(), "the request must emit trace output");
    for secret in SECRET_LATITUDES.iter().chain(&SECRET_LONGITUDES) {
        assert!(!text.contains(secret), "{secret} leaked into logs:\n{text}");
    }
}

#[tokio::test]
async fn journey_plan_logs_contain_no_request_coordinates() {
    let (logs, _guard) = capture_trace_logs();
    let body = json!({
        "origin": {
            "coordinate": {"latitude": 25.03817, "longitude": 121.56349},
            "display_text": "Origin"
        },
        "destination": {
            "coordinate": {"latitude": 25.04291, "longitude": 121.51872},
            "display_text": "Destination"
        },
        "selected_modes": ["bus", "bike_share"],
        "departure_setting": {"type": "depart_now"}
    });

    send(
        Request::builder()
            .method("POST")
            .uri("/v1/journey-plans")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;

    assert_no_coordinates(&logs);
}

#[tokio::test]
async fn rejected_journey_plan_logs_contain_no_request_coordinates() {
    let (logs, _guard) = capture_trace_logs();
    let body = json!({
        "origin": {
            "coordinate": {"latitude": 125.03817, "longitude": 121.56349},
            "display_text": "Origin"
        },
        "destination": {
            "coordinate": {"latitude": 25.04291, "longitude": 121.51872},
            "display_text": "Destination"
        },
        "selected_modes": [],
        "departure_setting": {"type": "depart_now"}
    });

    let status = send(
        Request::builder()
            .method("POST")
            .uri("/v1/journey-plans")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_no_coordinates(&logs);
}

#[tokio::test]
async fn nearby_stops_logs_contain_no_request_coordinates() {
    let (logs, _guard) = capture_trace_logs();

    let status = send(
        Request::builder()
            .uri("/v1/nearby-stops?latitude=25.03911&longitude=121.53407&radius_metres=2000")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_no_coordinates(&logs);
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = format!("{error} {error:?}");
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(&format!(" {cause} {cause:?}"));
        source = cause.source();
    }
    text
}

async fn failing_upstream_error_text(token_status: StatusCode) -> (String, String) {
    let (logs, _guard) = capture_trace_logs();
    let app = Router::new()
        .route(
            "/",
            post(move || async move {
                if token_status.is_success() {
                    Json(json!({"access_token": "fixture-token", "expires_in": 300}))
                        .into_response()
                } else {
                    token_status.into_response()
                }
            }),
        )
        .route(
            "/{*path}",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );
    let client = spawn_tdx_fixture(app).await;

    let error = client
        .get_json::<serde_json::Value>(
            via_server::tdx::Dataset::Bike,
            "nearby",
            &[("latitude", "25.03911"), ("longitude", "121.53407")],
        )
        .await
        .unwrap_err();

    (error_chain(&error), logs.text())
}

#[tokio::test]
async fn upstream_api_errors_do_not_carry_request_query_values() {
    let (error, logs) = failing_upstream_error_text(StatusCode::OK).await;

    assert!(error.contains("TDX API request failed"));
    assert!(logs.contains("tdx upstream response") && logs.contains("content_length"));
    for secret in ["25.03911", "121.53407"] {
        assert!(
            !error.contains(secret),
            "{secret} leaked into error:\n{error}"
        );
        assert!(!logs.contains(secret), "{secret} leaked into logs:\n{logs}");
    }
}

#[tokio::test]
async fn upstream_oauth_errors_do_not_carry_request_query_values() {
    let (error, logs) = failing_upstream_error_text(StatusCode::INTERNAL_SERVER_ERROR).await;

    assert!(error.contains("TDX OAuth request failed"));
    for secret in ["25.03911", "121.53407"] {
        assert!(
            !error.contains(secret),
            "{secret} leaked into error:\n{error}"
        );
        assert!(!logs.contains(secret), "{secret} leaked into logs:\n{logs}");
    }
}

#[tokio::test]
async fn rejected_nearby_stops_logs_and_bodies_contain_no_request_coordinates() {
    let (logs, _guard) = capture_trace_logs();

    for uri in [
        "/v1/nearby-stops?latitude=95.03911&longitude=121.53407",
        "/v1/nearby-stops?latitude=25.03911&longitude=121.53407&radius_metres=99999",
        "/v1/nearby-stops?latitude=25.03911&longitude=121.53407x",
    ] {
        let app = build_test_router_with_network(
            Arc::new(FakeStreetRouter::builder().build()),
            network(),
        )
        .await
        .unwrap();
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&body);
        assert!(!body.contains("25.03911") && !body.contains("121.53407"));
    }

    assert_no_coordinates(&logs);
}

/// Serves a response that fails before the head (`None`: a send error, which carries the request
/// URL) or mid-body after the given delay (a body error).
fn truncated_body_app(delay: Option<Duration>) -> Router {
    Router::new()
        .route(
            "/",
            post(|| async { Json(json!({"access_token": "fixture-token", "expires_in": 300})) }),
        )
        .route(
            "/{*path}",
            get(move || async move {
                use futures_util::StreamExt;
                let lost = || std::io::Error::other("upstream connection lost");
                let chunks = match delay {
                    None => futures_util::stream::iter([Err(lost())]).boxed(),
                    Some(delay) => futures_util::stream::once(async {
                        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial"))
                    })
                    .chain(futures_util::stream::once(async move {
                        tokio::time::sleep(delay).await;
                        Err(lost())
                    }))
                    .boxed(),
                };
                Body::from_stream(chunks).into_response()
            }),
        )
}

fn assert_error_hides_coordinates(error: &str, logs: &CapturedLogs) {
    assert!(error.contains("TDX API request failed"), "{error}");
    for secret in ["25.03911", "121.53407"] {
        assert!(
            !error.contains(secret),
            "{secret} leaked into error:\n{error}"
        );
        let logs = logs.text();
        assert!(!logs.contains(secret), "{secret} leaked into logs:\n{logs}");
    }
}

async fn fetch_client_failure_hides_coordinates(delay: Option<Duration>) {
    let (logs, _guard) = capture_trace_logs();
    let client = spawn_tdx_fixture(truncated_body_app(delay)).await;

    let error = client
        .fetch_bytes(
            "nearby",
            &[("latitude", "25.03911"), ("longitude", "121.53407")],
        )
        .await
        .unwrap_err();

    assert_error_hides_coordinates(&error_chain(&error), &logs);
}

async fn resumable_download_failure_hides_coordinates(delay: Option<Duration>) {
    let (logs, _guard) = capture_trace_logs();
    let client = spawn_tdx_fixture(truncated_body_app(delay)).await;
    let path = std::env::temp_dir().join(format!("via-log-privacy-{}.bin", std::process::id()));

    let result = client
        .download_to_path(
            &format!(
                "{}feed?latitude=25.03911&longitude=121.53407",
                client.settings().api_base_url
            ),
            &path,
            true,
        )
        .await;
    let _ = std::fs::remove_file(&path);

    assert_error_hides_coordinates(&error_chain(&result.unwrap_err()), &logs);
}

#[tokio::test]
async fn fetch_client_send_errors_do_not_carry_request_query_values() {
    fetch_client_failure_hides_coordinates(None).await;
}

#[tokio::test]
async fn fetch_client_body_errors_do_not_carry_request_query_values() {
    fetch_client_failure_hides_coordinates(Some(Duration::from_millis(50))).await;
}

#[tokio::test]
async fn resumable_download_send_errors_do_not_carry_request_query_values() {
    resumable_download_failure_hides_coordinates(None).await;
}

#[tokio::test]
async fn resumable_download_chunk_errors_do_not_carry_request_query_values() {
    resumable_download_failure_hides_coordinates(Some(Duration::from_millis(50))).await;
}

#[tokio::test]
async fn unrouted_request_paths_are_not_logged() {
    for uri in [
        "/25.04777,121.51712",
        "/v1/nearby-stops/25.04777/121.51712",
        "/v1/nearby-stops/25%2E04777/121%2E51712",
        "/v1/nearby-stops/25.048/121.517",
    ] {
        let (logs, _guard) = capture_trace_logs();

        let status = send(Request::builder().uri(uri).body(Body::empty()).unwrap()).await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        let text = logs.text();
        assert!(text.contains("unmatched"), "{uri}: {text}");
        for digits in [
            "25.04777",
            "121.51712",
            "25%2E04777",
            "121%2E51712",
            "25.048",
            "121.517",
        ] {
            assert!(
                !text.contains(digits),
                "{digits} from {uri} leaked:\n{text}"
            );
        }
    }
}

#[tokio::test]
async fn routed_request_logs_the_route_template() {
    let (logs, _guard) = capture_trace_logs();

    send(
        Request::builder()
            .uri("/v1/nearby-stops?latitude=25.03911&longitude=121.53407&radius_metres=2000")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert!(logs.text().contains("/v1/nearby-stops"), "{}", logs.text());
}

#[tokio::test]
async fn method_not_allowed_logs_the_route_template() {
    let (logs, _guard) = capture_trace_logs();

    let status = send(
        Request::builder()
            .method("DELETE")
            .uri("/v1/nearby-stops?latitude=25.03911&longitude=121.53407")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let text = logs.text();
    assert!(text.contains("/v1/nearby-stops"), "{text}");
    assert!(!text.contains("unmatched"), "{text}");
}

#[tokio::test]
async fn trailing_slash_path_is_logged_as_unmatched() {
    let (logs, _guard) = capture_trace_logs();

    let status = send(
        Request::builder()
            .uri("/v1/nearby-stops/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    let text = logs.text();
    assert!(text.contains("unmatched"), "{text}");
    assert!(!text.contains("/v1/nearby-stops"), "{text}");
}
