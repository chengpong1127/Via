use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use time::{OffsetDateTime, macros::datetime};
use tower::ServiceExt;
use via_server::{
    app::build_router_with_services,
    live_status::{Freshness, FreshnessState},
    refresh::{FeedRefreshBatch, FeedSource, RefreshCoordinator},
    snapshot::{InMemorySnapshotStore, NetworkSnapshot},
    street::{FakeProvider, GraphRevision},
};

struct FreshFeed;

#[async_trait]
impl FeedSource for FreshFeed {
    fn capability(&self) -> &str {
        "bus_arrival_taipei"
    }

    async fn refresh(
        &self,
        evaluated_at: OffsetDateTime,
    ) -> Result<FeedRefreshBatch, via_server::live_status::FreshnessReason> {
        Ok(FeedRefreshBatch {
            freshness: Freshness {
                state: FreshnessState::Fresh,
                reason: None,
                policy_version: "1".into(),
                evaluated_at,
            },
            live_status: Vec::new(),
        })
    }
}

async fn app() -> axum::Router {
    let now = datetime!(2026-08-31 12:00 UTC);
    let store = Arc::new(InMemorySnapshotStore::new(NetworkSnapshot {
        id: "initial".into(),
        created_at: now,
        live_cutoff: now,
        static_network_revision: "static-1".into(),
        schedule_revision: "schedule-1".into(),
        freshness_policy_version: "1".into(),
        publication_version: 1,
        feed_evidence: Vec::new(),
        live_status: Vec::new(),
    }));
    let coordinator = RefreshCoordinator::new(store.clone(), vec![Arc::new(FreshFeed)], 4);
    build_router_with_services(
        Arc::new(FakeProvider::builder(GraphRevision::new("revision-a")).build()),
        store,
        Some(coordinator),
        Some("scheduler-secret".into()),
    )
    .await
    .unwrap()
}

fn request(token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/internal/v1/refresh")
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder
        .body(Body::from(r#"{"refresh_key":"2026-08-31T12:00"}"#))
        .unwrap()
}

#[tokio::test]
async fn refresh_endpoint_requires_its_scheduler_token() {
    let response = app().await.oneshot(request(None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_endpoint_is_idempotent_for_one_scheduler_key() {
    let app = app().await;
    let first = app
        .clone()
        .oneshot(request(Some("scheduler-secret")))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body: serde_json::Value =
        serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(first_body["outcome"], "published");

    let second = app
        .oneshot(request(Some("scheduler-secret")))
        .await
        .unwrap();
    let second_body: serde_json::Value =
        serde_json::from_slice(&second.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(second_body["outcome"], "already_handled");
}
