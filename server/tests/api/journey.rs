use std::sync::Arc;

use crate::support::build_test_router;
use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use via_server::street::{
    Coordinate, FakeStreetRouter, Maneuver, Metres, Seconds, StreetPath, StreetTravelMode,
};

fn coordinate(latitude: f64, longitude: f64) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

fn request_body() -> Value {
    json!({
        "origin": {
            "coordinate": {"latitude": 25.04776, "longitude": 121.51706},
            "display_text": "Taipei Main Station"
        },
        "destination": {
            "coordinate": {"latitude": 25.03396, "longitude": 121.56447},
            "display_text": "Taipei 101"
        },
        "selected_modes": ["bus", "metro"],
        "departure_setting": {"type": "depart_now"}
    })
}

#[tokio::test]
async fn journey_endpoint_returns_one_coherent_provider_neutral_result() {
    let origin = coordinate(25.04776, 121.51706);
    let destination = coordinate(25.03396, 121.56447);
    let provider = FakeStreetRouter::builder()
        .path(
            StreetTravelMode::Walking,
            origin,
            destination,
            StreetPath {
                duration: Seconds(620),
                distance: Metres(3_900),
                geometry: vec![origin, destination],
                maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
                attribution: vec![],
            },
        )
        .build();
    let app = build_test_router(Arc::new(provider)).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(request_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let response_request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["generated_at"].is_string());
    assert_eq!(body["journey_options"].as_array().unwrap().len(), 1);
    assert_eq!(body["journey_options"][0]["label"], "best_now");
    assert_eq!(body["journey_options"][0]["legs"][0]["mode"], "walking");
    assert!(response_request_id.starts_with("req_"));
    let serialized = body.to_string();
    assert!(!serialized.contains("request_id"));
    assert!(!serialized.contains("network_revision"));
    assert!(!serialized.contains("revision-a"));
    assert!(!serialized.contains("fake"));
}

#[tokio::test]
async fn invalid_request_returns_a_redacted_error_envelope() {
    let provider = FakeStreetRouter::builder().build();
    let app = build_test_router(Arc::new(provider)).await.unwrap();
    let mut invalid = request_body();
    invalid["origin"]["coordinate"]["latitude"] = json!(91);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(invalid.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let response_request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"]["code"], "invalid_coordinate");
    assert_eq!(body["error"]["field"], "/origin/coordinate/latitude");
    assert_eq!(body.as_object().unwrap().len(), 1);
    assert_eq!(body["error"].as_object().unwrap().len(), 2);
    assert!(response_request_id.starts_with("req_"));
    assert!(body.get("request_id").is_none());
    assert!(!body.to_string().contains("Taipei Main Station"));
}

#[tokio::test]
async fn supplied_request_id_is_preserved_in_response_header() {
    let provider = FakeStreetRouter::builder().build();
    let app = build_test_router(Arc::new(provider)).await.unwrap();
    let request_id = "req_existing-correlation-id";

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("x-request-id", request_id)
                .header("content-type", "application/json")
                .body(Body::from(request_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.headers()["x-request-id"], request_id);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body.get("request_id").is_none());
}

#[tokio::test]
async fn successful_empty_result_is_distinct_from_service_unavailability() {
    let provider = FakeStreetRouter::builder().build();
    let app = build_test_router(Arc::new(provider)).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(request_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");
    assert!(body["generated_at"].is_string());
    assert_eq!(body["empty"].as_object().unwrap().len(), 1);
    assert!(body.get("journey_options").is_none());
}
