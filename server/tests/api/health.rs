use std::sync::Arc;

use crate::support::{build_test_router, build_test_router_with_network};
use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use via_server::{
    domain::ids::BoardingPointId,
    domain::{
        location::GeoCoordinate,
        transit::{TransitNetwork, TransitStop},
    },
    street::FakeStreetRouter,
};

#[tokio::test]
async fn liveness_reports_the_process_as_alive_without_caching() {
    let provider = Arc::new(FakeStreetRouter::builder().build());
    let app = build_test_router(provider).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["alive"], true);
}

#[tokio::test]
async fn axum_receives_the_provider_by_injection_and_reports_provider_name() {
    let provider = Arc::new(FakeStreetRouter::builder().build());
    let app = build_test_router(provider).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/street-routing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["provider"], "fake");
    assert_eq!(body["healthy"], true);
    assert_eq!(body.as_object().unwrap().len(), 2);
}

#[tokio::test]
async fn health_fails_when_the_street_dependency_is_unhealthy() {
    let provider = Arc::new(FakeStreetRouter::builder().unhealthy().build());
    let app = build_test_router(provider).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/street-routing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["healthy"], false);
    assert_eq!(body["provider"], "fake");
}

#[tokio::test]
async fn readiness_uses_the_immutable_static_network_without_a_snapshot() {
    let provider = Arc::new(FakeStreetRouter::builder().build());
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![TransitStop::new(
            BoardingPointId::new("stop-1"),
            "Stop",
            GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )],
        Vec::new(),
        Vec::new(),
    )
    .with_revisions("static-1", "schedule-1");
    let app = build_test_router_with_network(provider, network)
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["ready"], true);
    assert_eq!(body["static_network"], true);
    assert_eq!(body["transit_artifact"], true);
    assert!(body.get("network_snapshot").is_none());
}
