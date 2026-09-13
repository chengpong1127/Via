use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use serde_json::json;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    domain::ids::{BikeShareStationId, BoardingPointId, TransitServiceId},
    providers::realtime::{ArrivalQuery, RealtimeError, RealtimeProvider, RealtimeValue},
    providers::tdx::{IdentityResolver, TdxRealtimeConfig, TdxRealtimeProvider},
};

#[derive(Clone)]
struct FixtureIdentities {
    upstream_to_via: HashMap<(&'static str, &'static str), &'static str>,
    via_to_upstream: HashMap<(&'static str, &'static str), &'static str>,
}

#[async_trait]
impl IdentityResolver for FixtureIdentities {
    async fn resolve(&self, entity_kind: &str, _: &str, upstream_key: &str) -> Option<String> {
        self.upstream_to_via
            .get(&(entity_kind, upstream_key))
            .map(|value| (*value).to_owned())
    }

    async fn upstream_key(&self, entity_kind: &str, _: &str, via_id: &str) -> Option<String> {
        self.via_to_upstream
            .get(&(entity_kind, via_id))
            .map(|value| (*value).to_owned())
    }
}

async fn fixture_base(app: Router) -> url::Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url::Url::parse(&format!("http://{address}/")).unwrap()
}

fn identities() -> FixtureIdentities {
    FixtureIdentities {
        upstream_to_via: HashMap::from([
            (("boarding_point", "provider-stop"), "via-stop"),
            (("transit_service", "provider-service"), "via-service"),
            (("bike_share_station", "provider-station"), "via-station"),
        ]),
        via_to_upstream: HashMap::from([
            (("boarding_point", "via-stop"), "provider-stop"),
            (("transit_service", "via-service"), "provider-service"),
            (("bike_share_station", "via-station"), "provider-station"),
        ]),
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
    let source = TdxRealtimeProvider::new(
        TdxRealtimeConfig {
            base_url: fixture_base(app).await,
            authority_namespace: "taipei".into(),
        },
        "fixture-token",
    )
    .unwrap()
    .with_endpoint_paths("feed", "feed")
    .with_identity_resolver(Arc::new(identities()));

    let values = source
        .arrival_estimates(ArrivalQuery::for_ride(
            TransitServiceId::new("via-service"),
            BoardingPointId::new("via-stop"),
        ))
        .await
        .unwrap();
    assert_eq!(values.len(), 1);
    let RealtimeValue::ArrivalEstimate(arrival) = &values[0].value else {
        panic!("expected normalized arrival estimate");
    };
    assert_eq!(arrival.transit_service_id.as_str(), "via-service");
    assert_eq!(arrival.boarding_point_id.as_str(), "via-stop");
    assert_eq!(
        arrival.predicted_arrival_at,
        Some(values[0].value.value_arrival_time())
    );
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
    let source = TdxRealtimeProvider::new(
        TdxRealtimeConfig {
            base_url: fixture_base(app).await,
            authority_namespace: "taipei".into(),
        },
        "fixture-token",
    )
    .unwrap()
    .with_endpoint_paths("feed", "feed")
    .with_identity_resolver(Arc::new(identities()));

    let values = source
        .arrival_estimates(ArrivalQuery::for_ride(
            TransitServiceId::new("via-service"),
            BoardingPointId::new("via-stop"),
        ))
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
    let source = TdxRealtimeProvider::new(
        TdxRealtimeConfig {
            base_url: fixture_base(app).await,
            authority_namespace: "taipei".into(),
        },
        "fixture-token",
    )
    .unwrap()
    .with_endpoint_paths("feed", "feed")
    .with_identity_resolver(Arc::new(identities()));

    let values = source
        .bike_availability(&[BikeShareStationId::new("via-station")])
        .await
        .unwrap();
    let RealtimeValue::BikeAvailability(availability) = &values[0].value else {
        panic!("expected normalized bike availability");
    };
    assert_eq!(availability.station_id.as_str(), "via-station");
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
    let source = TdxRealtimeProvider::new(
        TdxRealtimeConfig {
            base_url: fixture_base(app).await,
            authority_namespace: "taipei".into(),
        },
        "fixture-token",
    )
    .unwrap()
    .with_service_condition_path("conditions")
    .with_identity_resolver(Arc::new(identities()));

    let values = source
        .service_conditions(&[TransitServiceId::new("via-service")])
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
    assert_eq!(condition.scope_via_ids, vec!["via-service"]);
    assert_eq!(condition.source_text_variants[0].text, "Service paused");
}

#[tokio::test]
async fn tdx_unknown_identity_is_rejected_without_leaking_provider_ids() {
    let update_time = (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap();
    let app = Router::new().route(
        "/feed",
        get(move || {
            let update_time = update_time.clone();
            async move {
                Json(json!([{
                    "StopUID": "unknown-stop",
                    "SubRouteUID": "unknown-service",
                    "StopStatus": 0,
                    "EstimateTime": 120,
                    "UpdateTime": update_time
                }]))
            }
        }),
    );
    let source = TdxRealtimeProvider::new(
        TdxRealtimeConfig {
            base_url: fixture_base(app).await,
            authority_namespace: "taipei".into(),
        },
        "fixture-token",
    )
    .unwrap()
    .with_endpoint_paths("feed", "feed")
    .with_identity_resolver(Arc::new(identities()));

    let error = source
        .arrival_estimates(ArrivalQuery::for_ride(
            TransitServiceId::new("via-service"),
            BoardingPointId::new("via-stop"),
        ))
        .await
        .unwrap_err();
    assert_eq!(error, RealtimeError::UnknownIdentity);
}

#[tokio::test]
async fn tdx_rate_limits_are_explicit_realtime_errors() {
    let app = Router::new().route(
        "/feed",
        get(|| async { StatusCode::TOO_MANY_REQUESTS.into_response() }),
    );
    let source = TdxRealtimeProvider::new(
        TdxRealtimeConfig {
            base_url: fixture_base(app).await,
            authority_namespace: "taipei".into(),
        },
        "fixture-token",
    )
    .unwrap()
    .with_endpoint_paths("feed", "feed")
    .with_identity_resolver(Arc::new(identities()));
    let error = source
        .bike_availability(&[BikeShareStationId::new("via-station")])
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
