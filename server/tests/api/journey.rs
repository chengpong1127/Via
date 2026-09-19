use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use via_server::{
    app::build_router,
    street::{
        Coordinate, FakeStreetRouter, Maneuver, Metres, Seconds, StreetPath, StreetTravelMode,
    },
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
        "locale": "en",
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
    let app = build_router(Arc::new(provider)).await.unwrap();

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
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["api_version"], "1.0");
    assert_eq!(body["journey_options"].as_array().unwrap().len(), 1);
    assert_eq!(body["journey_options"][0]["class"], "reliable");
    assert_eq!(body["journey_options"][0]["label"], "best_now");
    assert_eq!(body["journey_options"][0]["legs"][0]["mode"], "walking");
    assert_eq!(
        body["network_revision"]["static_network_revision"],
        "development-static"
    );
    assert_eq!(
        body["network_revision"]["schedule_revision"],
        "development-schedule"
    );
    let serialized = body.to_string();
    assert!(!serialized.contains("revision-a"));
    assert!(!serialized.contains("fake"));
}

#[tokio::test]
async fn invalid_request_returns_a_redacted_error_envelope() {
    let provider = FakeStreetRouter::builder().build();
    let app = build_router(Arc::new(provider)).await.unwrap();
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
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"]["code"], "invalid_coordinate");
    assert_eq!(body["error"]["field"], "/origin/coordinate/latitude");
    assert!(!body.to_string().contains("Taipei Main Station"));
}

#[tokio::test]
async fn successful_empty_result_is_distinct_from_service_unavailability() {
    let provider = FakeStreetRouter::builder().build();
    let app = build_router(Arc::new(provider)).await.unwrap();

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
    assert!(body.get("journey_options").is_none());
}
