use std::sync::Arc;

use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use serde_json::json;
use time::macros::datetime;
use via_server::{
    live_status::{FreshnessReason, FreshnessState},
    refresh::FeedSource,
    tdx::{TdxFeedConfig, TdxFeedSource},
};

async fn fixture_url(app: Router) -> url::Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url::Url::parse(&format!("http://{address}/feed")).unwrap()
}

#[tokio::test]
async fn tdx_feed_uses_provider_publication_time_for_freshness() {
    let app = Router::new().route(
        "/feed",
        get(|| async {
            Json(json!([{
                "StationUID": "provider-id-stays-in-adapter",
                "SrcUpdateTime": "2026-08-01T00:00:00+08:00",
                "UpdateTime": "2026-08-31T11:59:30+08:00"
            }]))
        }),
    );
    let source = TdxFeedSource::new(
        TdxFeedConfig {
            capability: "bike_availability_taipei".into(),
            url: fixture_url(app).await,
        },
        "fixture-token",
    )
    .unwrap();

    let freshness = source
        .refresh(datetime!(2026-08-31 12:00 +08:00))
        .await
        .unwrap();
    assert_eq!(freshness.state, FreshnessState::Fresh);
}

#[tokio::test]
async fn tdx_rate_limits_are_explicit_degradation_evidence() {
    let app = Router::new().route(
        "/feed",
        get(|| async { StatusCode::TOO_MANY_REQUESTS.into_response() }),
    );
    let source = Arc::new(
        TdxFeedSource::new(
            TdxFeedConfig {
                capability: "bus_arrival_taipei".into(),
                url: fixture_url(app).await,
            },
            "fixture-token",
        )
        .unwrap(),
    );

    let error = source
        .refresh(datetime!(2026-08-31 12:00 +08:00))
        .await
        .unwrap_err();
    assert_eq!(error, FreshnessReason::RateLimited);
}
