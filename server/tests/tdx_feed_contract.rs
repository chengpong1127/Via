use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use serde_json::json;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    live_status::{FreshnessReason, FreshnessState},
    refresh::FeedSource,
    snapshot::LiveStatusValue,
    tdx::{IdentityResolver, TdxFeedConfig, TdxFeedFamily, TdxFeedSource},
};

struct FixtureIdentities(HashMap<(&'static str, &'static str), &'static str>);

#[async_trait]
impl IdentityResolver for FixtureIdentities {
    async fn resolve(&self, entity_kind: &str, _: &str, upstream_key: &str) -> Option<String> {
        self.0
            .get(&(entity_kind, upstream_key))
            .map(|value| (*value).to_owned())
    }
}

async fn fixture_url(app: Router) -> url::Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url::Url::parse(&format!("http://{address}/feed")).unwrap()
}

#[tokio::test]
async fn tdx_feed_uses_provider_publication_time_for_freshness() {
    let update_time = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/feed",
        get(move || {
            let update_time = update_time.clone();
            async move {
                Json(json!([{
                    "StationUID": "provider-id-stays-in-adapter",
                    "ServiceStatus": 1,
                    "AvailableRentBikes": 7,
                    "AvailableReturnBikes": 9,
                    "SrcUpdateTime": "2026-08-01T00:00:00+08:00",
                    "UpdateTime": update_time
                }]))
            }
        }),
    );
    let source = TdxFeedSource::new(
        TdxFeedConfig {
            capability: "bike_availability_taipei".into(),
            url: fixture_url(app).await,
            family: TdxFeedFamily::BikeAvailability,
            authority_namespace: "taipei".into(),
        },
        "fixture-token",
    )
    .unwrap()
    .with_identity_resolver(Arc::new(FixtureIdentities(HashMap::from([(
        ("youbike_station", "provider-id-stays-in-adapter"),
        "via_youbike_1",
    )]))));

    let batch = source.refresh(OffsetDateTime::now_utc()).await.unwrap();
    assert_eq!(batch.freshness.state, FreshnessState::Fresh);
    assert_eq!(batch.live_status.len(), 1);
    let LiveStatusValue::BikeAvailability(availability) = &batch.live_status[0].value else {
        panic!("expected normalized bike availability");
    };
    assert_eq!(availability.station_id.as_str(), "via_youbike_1");
    assert_eq!(availability.available_bikes, Some(7));
    assert_eq!(availability.available_return_slots, Some(9));
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
                family: TdxFeedFamily::BusArrival,
                authority_namespace: "taipei".into(),
            },
            "fixture-token",
        )
        .unwrap(),
    );

    let error = source.refresh(OffsetDateTime::now_utc()).await.unwrap_err();
    assert_eq!(error, FreshnessReason::RateLimited);
}
