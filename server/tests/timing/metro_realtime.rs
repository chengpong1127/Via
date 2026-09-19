use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::support::tdx::spawn_tdx_fixture;
use axum::{
    Json, Router,
    http::{Method, StatusCode, Uri},
    response::IntoResponse,
    routing::any,
};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    domain::{
        ids::{BoardingPointId, TransitServiceId},
        transit::{
            MetroStationTiming, MetroTiming, TransitMode, TransitNetwork, TransitService,
            TransitStop,
        },
        transit_timing::{DepartureQuery, DepartureResolver, TimingBasis, TimingReliability},
    },
    providers::timing::{DepartureDispatcher, MetroDepartureSource, TransitDepartureSource},
    tdx::TdxClient,
};

#[derive(Clone)]
struct FixtureStats {
    calls: Arc<AtomicUsize>,
}

async fn fixture_client(payload: Value, status: StatusCode) -> (Arc<TdxClient>, FixtureStats) {
    let stats = FixtureStats {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let calls = stats.calls.clone();
    let app = Router::new().fallback(any(move |method: Method, _uri: Uri| {
        let calls = calls.clone();
        let payload = payload.clone();
        async move {
            if method == Method::POST {
                return (
                    StatusCode::OK,
                    Json(json!({
                        "access_token": "fixture-token",
                        "expires_in": 300
                    })),
                )
                    .into_response();
            }
            calls.fetch_add(1, Ordering::Relaxed);
            if status != StatusCode::OK {
                return status.into_response();
            }
            (status, Json(payload)).into_response()
        }
    }));
    (spawn_tdx_fixture(app).await, stats)
}

fn recent_update() -> String {
    (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap()
}

fn live_board(line: &str, station: &str, destination: &str, update: &str) -> Value {
    json!({
        "LineID": line,
        "LineName": {"Zh_tw": "板南線"},
        "StationID": station,
        "StationName": {"Zh_tw": "台北車站"},
        "TripHeadSign": "南港展覽館",
        "DestinationStaionID": destination,
        "DestinationStationID": destination,
        "DestinationStationName": {"Zh_tw": "南港展覽館"},
        "ServiceStatus": 0,
        "EstimateTime": 1,
        "SrcUpdateTime": update,
        "UpdateTime": update
    })
}

fn network_for(static_departure: OffsetDateTime) -> Arc<TransitNetwork> {
    let first = BoardingPointId::new("TRTC-BL12");
    let second = BoardingPointId::new("TRTC-BL23");
    let service_id = TransitServiceId::new("metro:BL::direction=0");
    let seed = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let service_day_start = seed.service_day_start(seed.service_day(static_departure));
    let static_seconds =
        u32::try_from((static_departure - service_day_start).whole_seconds()).unwrap();
    Arc::new(
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![
                TransitStop::new(
                    first.clone(),
                    "Taipei Main Station",
                    via_server::api::RequestCoordinate {
                        latitude: 25.0478,
                        longitude: 121.5170,
                    },
                ),
                TransitStop::new(
                    second.clone(),
                    "Longshan Temple",
                    via_server::api::RequestCoordinate {
                        latitude: 25.0354,
                        longitude: 121.4998,
                    },
                ),
            ],
            vec![TransitService::new(
                service_id.clone(),
                TransitMode::Metro,
                "板南線",
                "南港展覽館",
                vec![first.clone(), second.clone()],
            )],
            Vec::new(),
        )
        .with_metro_timings(vec![MetroTiming::new(
            service_id,
            vec![MetroStationTiming {
                stop_id: first,
                departures_seconds: vec![static_seconds],
                frequencies: Vec::new(),
            }],
            vec![240],
        )])
        .with_revisions("static", "schedule"),
    )
}

fn query(after: OffsetDateTime) -> DepartureQuery {
    DepartureQuery::new(
        TransitServiceId::new("metro:BL::direction=0"),
        BoardingPointId::new("TRTC-BL12"),
        after,
        2,
    )
}

async fn resolver(
    network: Arc<TransitNetwork>,
    payload: Value,
    status: StatusCode,
) -> (DepartureDispatcher, FixtureStats) {
    let (tdx, stats) = fixture_client(payload, status).await;
    let metro = Arc::new(
        MetroDepartureSource::new(network.clone())
            .with_tdx(Some(tdx))
            .with_realtime_endpoint("v2/Rail/Metro/LiveBoard/TRTC"),
    );
    (
        DepartureDispatcher::new(network, vec![metro as Arc<dyn TransitDepartureSource>]),
        stats,
    )
}

#[tokio::test]
async fn fresh_metro_realtime_overrides_static_and_materializes_downstream_times() {
    let after = OffsetDateTime::now_utc() - Duration::seconds(10);
    let static_departure = after + Duration::minutes(5);
    let update = recent_update();
    let network = network_for(static_departure);
    let (resolver, _) = resolver(
        network,
        Value::Array(vec![live_board("BL", "BL12", "BL23", &update)]),
        StatusCode::OK,
    )
    .await;

    let departures = resolver.next_departures(query(after)).await.unwrap();

    assert_eq!(departures.len(), 1);
    assert_eq!(departures[0].timing_basis, TimingBasis::RealtimePrediction);
    assert_eq!(
        departures[0].timing_reliability,
        TimingReliability::Reliable
    );
    let observed_at = OffsetDateTime::parse(&update, &Rfc3339).unwrap();
    assert_eq!(
        departures[0].stop_times[0].departure_at,
        observed_at + Duration::minutes(1)
    );
    assert_eq!(
        departures[0].stop_times[1].arrival_at,
        observed_at + Duration::minutes(1) + Duration::seconds(240)
    );
    assert_eq!(departures[0].observed_at, Some(observed_at));
}

#[tokio::test]
async fn metro_realtime_matches_station_line_and_direction() {
    let after = OffsetDateTime::now_utc() - Duration::seconds(10);
    let update = recent_update();
    let payload = Value::Array(vec![
        live_board("R", "BL12", "BL23", &update),
        live_board("BL", "BL23", "BL23", &update),
        live_board("BL", "BL12", "BL12", &update),
        live_board("BL", "BL12", "BL23", &update),
    ]);
    let (resolver, _) = resolver(
        network_for(after + Duration::minutes(5)),
        payload,
        StatusCode::OK,
    )
    .await;

    let departures = resolver.next_departures(query(after)).await.unwrap();

    assert_eq!(departures.len(), 1);
    assert_eq!(departures[0].timing_basis, TimingBasis::RealtimePrediction);
}

#[tokio::test]
async fn stale_or_missing_metro_realtime_falls_back_to_static() {
    let after = OffsetDateTime::now_utc() - Duration::seconds(10);
    let static_departure = after + Duration::minutes(5);
    let stale = (OffsetDateTime::now_utc() - Duration::seconds(400))
        .format(&Rfc3339)
        .unwrap();
    for payload in [
        Value::Array(vec![live_board("BL", "BL12", "BL23", &stale)]),
        Value::Array(Vec::new()),
    ] {
        let (resolver, _) = resolver(network_for(static_departure), payload, StatusCode::OK).await;
        let departures = resolver.next_departures(query(after)).await.unwrap();
        assert_eq!(departures.len(), 1);
        assert_eq!(departures[0].timing_basis, TimingBasis::Schedule);
        assert_eq!(
            departures[0].timing_reliability,
            TimingReliability::Fallback
        );
    }
}

#[tokio::test]
async fn malformed_or_failed_metro_realtime_falls_back_safely() {
    let after = OffsetDateTime::now_utc() - Duration::seconds(10);
    let static_departure = after + Duration::minutes(5);
    let malformed = json!([{
        "LineID": "BL",
        "StationID": "BL12",
        "ServiceStatus": 0,
        "EstimateTime": "soon",
        "SrcUpdateTime": recent_update(),
        "UpdateTime": recent_update()
    }]);
    for (payload, status) in [
        (malformed, StatusCode::OK),
        (Value::Null, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let (resolver, _) = resolver(network_for(static_departure), payload, status).await;
        let departures = resolver.next_departures(query(after)).await.unwrap();
        assert_eq!(departures[0].timing_basis, TimingBasis::Schedule);
        assert_eq!(
            departures[0].timing_reliability,
            TimingReliability::Fallback
        );
    }
}

#[tokio::test]
async fn duplicate_metro_queries_use_one_batched_tdx_request() {
    let after = OffsetDateTime::now_utc() - Duration::seconds(10);
    let update = recent_update();
    let (resolver, stats) = resolver(
        network_for(after + Duration::minutes(5)),
        Value::Array(vec![live_board("BL", "BL12", "BL23", &update)]),
        StatusCode::OK,
    )
    .await;
    let request = query(after);

    let results = resolver
        .next_departures_batch(vec![request.clone(), request])
        .await;

    assert_eq!(stats.calls.load(Ordering::Relaxed), 1);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| {
        result
            .as_ref()
            .is_ok_and(|trips| trips[0].timing_basis == TimingBasis::RealtimePrediction)
    }));
}
