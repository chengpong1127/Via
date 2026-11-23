use std::sync::Arc;

use crate::support::{
    build_test_router_with_network,
    log_capture::{CapturedLogs, capture_trace_logs},
    tdx::spawn_tdx_fixture,
};
use axum::{
    Json, Router,
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

const SECRET_LATITUDES: [&str; 3] = ["25.03817", "25.04291", "25.03911"];
const SECRET_LONGITUDES: [&str; 3] = ["121.56349", "121.51872", "121.53407"];

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

#[tokio::test]
async fn upstream_http_errors_do_not_carry_request_query_values() {
    let app = Router::new()
        .route(
            "/",
            post(|| async { Json(json!({"access_token": "fixture-token", "expires_in": 300})) }),
        )
        .route(
            "/{*path}",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );
    let client = spawn_tdx_fixture(app).await;

    let error = client
        .get_json::<serde_json::Value>(
            "nearby",
            &[("latitude", "25.03911"), ("longitude", "121.53407")],
        )
        .await
        .unwrap_err();

    let text = error_chain(&error);
    assert!(text.contains("TDX API request failed"));
    for secret in ["25.03911", "121.53407"] {
        assert!(
            !text.contains(secret),
            "{secret} leaked into error:\n{text}"
        );
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
