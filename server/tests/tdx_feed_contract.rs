use std::collections::HashMap;

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
    domain::ids::{BikeShareStationId, BoardingPointId, TransitServiceId},
    providers::realtime::{ArrivalQuery, RealtimeError, RealtimeProvider, RealtimeValue},
    providers::tdx::{TdxRealtimeConfig, TdxRealtimeProvider, transit_service_id_for_direction},
};

async fn fixture_base(app: Router) -> url::Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url::Url::parse(&format!("http://{address}/")).unwrap()
}

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
    let base_url = fixture_base(app).await;
    let client = TdxClient::with_urls(
        TdxCredentials::from_client_credentials("fixture-client", "fixture-secret"),
        base_url.as_str(),
        base_url.as_str(),
    )
    .unwrap();
    TdxRealtimeProvider::new(TdxRealtimeConfig { base_url }, client).unwrap()
}

#[tokio::test]
async fn tdx_default_arrival_endpoint_uses_the_explicit_city_scope() {
    let update_time = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/v2/Bus/EstimatedTimeOfArrival/City/NewTaipei",
        get(
            move |query: axum::extract::Query<HashMap<String, String>>| {
                let update_time = update_time.clone();
                async move {
                    assert!(query.get("$filter").is_some());
                    Json(json!([{
                        "StopUID": "provider-stop",
                        "SubRouteUID": "provider-service",
                        "StopStatus": 0,
                        "EstimateTime": 120,
                        "UpdateTime": update_time
                    }]))
                }
            },
        ),
    );
    let source = fixture_provider(app).await;

    let values = source
        .arrival_estimates(
            ArrivalQuery::for_ride(
                TransitServiceId::new("provider-service"),
                BoardingPointId::new("provider-stop"),
            )
            .with_scope("NewTaipei"),
        )
        .await
        .unwrap();

    assert_eq!(values.len(), 1);
}

#[tokio::test]
async fn tdx_arrivals_reject_missing_and_invalid_scopes_before_http() {
    let source = fixture_provider(Router::new()).await;
    let service_id = TransitServiceId::new("provider-service");
    let boarding_point_id = BoardingPointId::new("provider-stop");

    for query in [
        ArrivalQuery::for_ride(service_id.clone(), boarding_point_id.clone()),
        ArrivalQuery::for_ride(service_id.clone(), boarding_point_id.clone()).with_scope(""),
        ArrivalQuery::for_ride(service_id, boarding_point_id).with_scope("../Taipei"),
    ] {
        assert_eq!(
            source.arrival_estimates(query).await,
            Err(RealtimeError::InvalidConfiguration)
        );
    }
}

#[tokio::test]
async fn tdx_arrivals_use_a_scoped_query_and_provider_timestamps() {
    let update_time = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/feed",
        get(
            move |query: axum::extract::Query<HashMap<String, String>>| {
                let update_time = update_time.clone();
                async move {
                    assert!(query.get("$filter").is_some());
                    Json(json!([{
                        "StopUID": "provider-stop",
                        "SubRouteUID": "provider-service",
                        "StopStatus": 0,
                        "EstimateTime": 120,
                        "UpdateTime": update_time
                    }]))
                }
            },
        ),
    );
    let source = fixture_provider(app)
        .await
        .with_endpoint_paths("feed", "feed");

    let values = source
        .arrival_estimates(
            ArrivalQuery::for_ride(
                TransitServiceId::new("provider-service"),
                BoardingPointId::new("provider-stop"),
            )
            .with_scope("Taipei"),
        )
        .await
        .unwrap();
    assert_eq!(values.len(), 1);
    let RealtimeValue::ArrivalEstimate(arrival) = &values[0].value else {
        panic!("expected normalized arrival estimate");
    };
    assert_eq!(arrival.transit_service_id.as_str(), "provider-service");
    assert_eq!(arrival.boarding_point_id.as_str(), "provider-stop");
    assert_eq!(
        arrival.predicted_arrival_at,
        Some(values[0].value.value_arrival_time())
    );
}

#[tokio::test]
async fn directional_tdx_service_ids_match_static_directional_ids() {
    let update_time = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/feed",
        get(
            move |query: axum::extract::Query<HashMap<String, String>>| {
                let update_time = update_time.clone();
                async move {
                    assert!(query.get("$filter").is_some_and(|filter| {
                        filter.contains("provider-service") && !filter.contains("::direction=")
                    }));
                    Json(json!([{
                        "StopUID": "provider-stop",
                        "SubRouteUID": "provider-service",
                        "Direction": 1,
                        "StopStatus": 0,
                        "EstimateTime": 120,
                        "UpdateTime": update_time
                    }]))
                }
            },
        ),
    );
    let source = fixture_provider(app)
        .await
        .with_endpoint_paths("feed", "feed");
    let service_id = transit_service_id_for_direction("provider-service", 1);
    let values = source
        .arrival_estimates(
            ArrivalQuery::for_ride(service_id.clone(), BoardingPointId::new("provider-stop"))
                .with_scope("Taipei"),
        )
        .await
        .unwrap();

    let RealtimeValue::ArrivalEstimate(arrival) = &values[0].value else {
        panic!("expected normalized arrival estimate");
    };
    assert_eq!(arrival.transit_service_id, service_id);
}

#[tokio::test]
async fn tdx_evaluates_freshness_per_record_in_a_scoped_response() {
    let fresh_update = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let stale_update = (OffsetDateTime::now_utc() - Duration::seconds(400))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/feed",
        get(move || {
            let fresh_update = fresh_update.clone();
            let stale_update = stale_update.clone();
            async move {
                Json(json!([
                    {
                        "StopUID": "provider-stop",
                        "SubRouteUID": "provider-service",
                        "StopStatus": 0,
                        "EstimateTime": 120,
                        "UpdateTime": fresh_update
                    },
                    {
                        "StopUID": "provider-stop",
                        "SubRouteUID": "provider-service",
                        "StopStatus": 0,
                        "EstimateTime": 120,
                        "UpdateTime": stale_update
                    }
                ]))
            }
        }),
    );
    let source = fixture_provider(app)
        .await
        .with_endpoint_paths("feed", "feed");

    let values = source
        .arrival_estimates(
            ArrivalQuery::for_ride(
                TransitServiceId::new("provider-service"),
                BoardingPointId::new("provider-stop"),
            )
            .with_scope("Taipei"),
        )
        .await
        .unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(
        values
            .iter()
            .map(|value| value.freshness.state)
            .collect::<Vec<_>>(),
        vec![
            via_server::domain::realtime::FreshnessState::Fresh,
            via_server::domain::realtime::FreshnessState::Unavailable
        ]
    );
}

#[tokio::test]
async fn tdx_bike_availability_is_normalized_inside_the_adapter() {
    let update_time = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/feed",
        get(move || {
            let update_time = update_time.clone();
            async move {
                Json(json!([{
                    "StationUID": "provider-station",
                    "ServiceStatus": 1,
                    "AvailableRentBikes": 7,
                    "AvailableReturnBikes": 9,
                    "SrcUpdateTime": "2026-08-01T00:00:00+08:00",
                    "UpdateTime": update_time
                }]))
            }
        }),
    );
    let source = fixture_provider(app)
        .await
        .with_endpoint_paths("feed", "feed");

    let values = source
        .bike_availability(&[BikeShareStationId::new("provider-station")])
        .await
        .unwrap();
    let RealtimeValue::BikeAvailability(availability) = &values[0].value else {
        panic!("expected normalized bike availability");
    };
    assert_eq!(availability.station_id.as_str(), "provider-station");
    assert_eq!(availability.available_bikes, Some(7));
    assert_eq!(availability.available_return_slots, Some(9));
    assert_eq!(
        values[0].freshness.state,
        via_server::domain::realtime::FreshnessState::Fresh
    );
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
    let RealtimeValue::ServiceCondition(condition) = &values[0].value else {
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
async fn tdx_rate_limits_are_explicit_realtime_errors() {
    let app = Router::new().route(
        "/feed",
        get(|| async { StatusCode::TOO_MANY_REQUESTS.into_response() }),
    );
    let source = fixture_provider(app)
        .await
        .with_endpoint_paths("feed", "feed");
    let error = source
        .bike_availability(&[BikeShareStationId::new("provider-station")])
        .await
        .unwrap_err();
    assert_eq!(error, RealtimeError::RateLimited);
}

trait ArrivalTime {
    fn value_arrival_time(&self) -> OffsetDateTime;
}

impl ArrivalTime for RealtimeValue {
    fn value_arrival_time(&self) -> OffsetDateTime {
        match self {
            RealtimeValue::ArrivalEstimate(value) => {
                value.transmitted_at + time::Duration::seconds(120)
            }
            _ => panic!("not an arrival"),
        }
    }
}
