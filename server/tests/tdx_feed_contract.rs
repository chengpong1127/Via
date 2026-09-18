use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::json;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    config::{TdxClient, TdxCredentials},
    domain::ids::TransitServiceId,
    providers::{TdxRealtimeProvider, realtime::RealtimeProvider},
};

async fn fixture_provider(app: Router) -> TdxRealtimeProvider {
    let app = app.route(
        "/",
        post(|| async {
            Json(json!({
                "access_token": "fixture-token",
                "expires_in": 300
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base_url = format!("http://{address}/");
    let client = Arc::new(
        TdxClient::with_urls(
            TdxCredentials::from_client_credentials("fixture-client", "fixture-secret"),
            &base_url,
            &base_url,
        )
        .unwrap(),
    );
    TdxRealtimeProvider::new(client)
}

#[tokio::test]
async fn tdx_service_conditions_are_scoped_and_normalized_inside_the_adapter() {
    let update_time = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/conditions",
        get(
            move |query: axum::extract::Query<HashMap<String, String>>| {
                let update_time = update_time.clone();
                async move {
                    assert!(
                        query
                            .get("$filter")
                            .is_some_and(|filter| filter.contains("provider-service"))
                    );
                    Json(json!([{
                        "SubRouteUID": "provider-service",
                        "Status": "suspended",
                        "Message": "Service paused",
                        "Language": "en",
                        "EffectiveFrom": "2026-08-31T09:00:00+08:00",
                        "UpdateTime": update_time
                    }]))
                }
            },
        ),
    );
    let source = fixture_provider(app)
        .await
        .with_service_condition_path("conditions");

    let values = source
        .service_conditions(&[TransitServiceId::new("provider-service")])
        .await
        .unwrap();
    assert_eq!(values.len(), 1);
    let via_server::providers::realtime::RealtimeValue::ServiceCondition(condition) =
        &values[0].value
    else {
        panic!("expected normalized service condition");
    };
    assert_eq!(
        condition.state,
        via_server::domain::realtime::ServiceConditionState::Suspended
    );
    assert_eq!(condition.scope_via_ids, vec!["provider-service"]);
    assert_eq!(condition.source_text_variants[0].text, "Service paused");
}

#[tokio::test]
async fn tdx_service_condition_rate_limits_are_explicit_errors() {
    let app = Router::new().route(
        "/conditions",
        get(|| async { StatusCode::TOO_MANY_REQUESTS.into_response() }),
    );
    let source = fixture_provider(app)
        .await
        .with_service_condition_path("conditions");
    let error = source
        .service_conditions(&[TransitServiceId::new("provider-service")])
        .await
        .unwrap_err();
    assert_eq!(
        error,
        via_server::providers::realtime::RealtimeError::RateLimited
    );
}
