use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use via_server::{
    app::{build_router, build_router_with_network},
    domain::ids::BoardingPointId,
    domain::{
        location::RequestCoordinate,
        transit::{TransitNetwork, TransitStop},
    },
    street::{FakeProvider, GraphRevision},
};

#[tokio::test]
async fn axum_receives_the_provider_by_injection_and_reports_its_pinned_revision() {
    let provider = Arc::new(FakeProvider::builder(GraphRevision::new("revision-a")).build());
    let app = build_router(provider).await.unwrap();

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
    assert_eq!(body["graph_revision"], "revision-a");
    assert_eq!(body["pinned_graph_revision"], "revision-a");
}

#[tokio::test]
async fn health_fails_when_publication_replaces_the_graph_under_a_running_server() {
    let provider = Arc::new(FakeProvider::builder(GraphRevision::new("revision-a")).build());
    let app = build_router(provider.clone()).await.unwrap();
    provider
        .publish_revision(GraphRevision::new("revision-b"))
        .await;

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
    assert_eq!(body["graph_revision"], "revision-b");
    assert_eq!(body["pinned_graph_revision"], "revision-a");
}

#[tokio::test]
async fn readiness_uses_the_immutable_static_network_without_a_snapshot() {
    let provider = Arc::new(FakeProvider::builder(GraphRevision::new("revision-a")).build());
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![TransitStop::new(
            BoardingPointId::new("stop-1"),
            "Stop",
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .with_revisions("static-1", "schedule-1");
    let app = build_router_with_network(provider, network).await.unwrap();

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
