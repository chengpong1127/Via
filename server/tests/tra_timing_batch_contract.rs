use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    http::{Method, StatusCode, Uri},
    response::IntoResponse,
    routing::any,
};
use serde_json::{Value, json};
use time::{
    Duration, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset, Weekday,
    format_description::well_known::Rfc3339,
};
use via_server::{
    domain::{
        ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
        transit::{ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService},
        transit_timing::{DepartureQuery, DepartureResolver, TimingBasis, TimingReliability},
    },
    providers::timing::{DepartureDispatcher, TraDepartureSource, TransitDepartureSource},
    tdx::{TdxClient, TdxCredentials},
};

#[derive(Clone)]
struct FixtureStats {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
}

async fn fixture_client(payload: Value, status: StatusCode) -> (Arc<TdxClient>, FixtureStats) {
    let stats = FixtureStats {
        calls: Arc::new(AtomicUsize::new(0)),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let calls = stats.calls.clone();
    let requests = stats.requests.clone();
    let app = Router::new().fallback(any(move |method: Method, uri: Uri| {
        let calls = calls.clone();
        let requests = requests.clone();
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
            requests.lock().unwrap().push(uri.to_string());
            if status != StatusCode::OK {
                return status.into_response();
            }
            (status, Json(payload)).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base_url = format!("http://{address}/");
    let client = TdxClient::with_urls(
        TdxCredentials::from_client_credentials("fixture-client", "fixture-secret"),
        &base_url,
        &base_url,
    )
    .unwrap();
    (Arc::new(client), stats)
}

fn recent_update() -> String {
    (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap()
}

fn after() -> OffsetDateTime {
    let taipei = UtcOffset::from_hms(8, 0, 0).unwrap();
    let now = OffsetDateTime::now_utc().to_offset(taipei);
    let service_day_boundary = Time::from_hms(4, 0, 0).unwrap();
    let service_day = if now.time() < service_day_boundary {
        now.date().previous_day().unwrap()
    } else {
        now.date()
    };
    PrimitiveDateTime::new(service_day, Time::from_hms(9, 0, 0).unwrap()).assume_offset(taipei)
}

fn expected_departure(delay_minutes: i64) -> OffsetDateTime {
    after().replace_time(Time::from_hms(10, 0, 0).unwrap()) + Duration::minutes(delay_minutes)
}

fn train(train_no: &str, delay_minutes: i64, update_time: &str) -> Value {
    json!({
        "TrainNo": train_no,
        "TrainTypeName": {"Zh_tw": "自強", "En": "Tze-Chiang"},
        "StationName": {"Zh_tw": "台北", "En": "Taipei"},
        "DelayTime": delay_minutes,
        "UpdateTime": update_time
    })
}

fn payload(trains: Vec<Value>, update_time: &str) -> Value {
    json!({
        "UpdateTime": update_time,
        "UpdateInterval": 30,
        "SrcUpdateTime": update_time,
        "SrcUpdateInterval": 30,
        "AuthorityCode": "TRA",
        "TrainLiveBoards": trains
    })
}

fn network_for(train_numbers: &[&str]) -> Arc<TransitNetwork> {
    let board = BoardingPointId::new("stop-a");
    let alight = BoardingPointId::new("stop-b");
    let services = train_numbers
        .iter()
        .map(|train_no| {
            TransitService::new(
                TransitServiceId::new(format!("tra:{train_no}")),
                TransitMode::Rail,
                format!("TRA {train_no}"),
                "Taipei",
                vec![board.clone(), alight.clone()],
            )
        })
        .collect::<Vec<_>>();
    let trips = train_numbers
        .iter()
        .map(|train_no| {
            ScheduledTrip::weekdays(
                ScheduledTripId::new(format!("scheduled-{train_no}")),
                TransitServiceId::new(format!("tra:{train_no}")),
                vec![
                    Weekday::Monday,
                    Weekday::Tuesday,
                    Weekday::Wednesday,
                    Weekday::Thursday,
                    Weekday::Friday,
                    Weekday::Saturday,
                    Weekday::Sunday,
                ],
                vec![
                    ScheduledStopTime::new(36_000, 36_000),
                    ScheduledStopTime::new(36_600, 36_600),
                ],
            )
        })
        .collect::<Vec<_>>();
    Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        services,
        trips,
    ))
}

fn query(train_no: &str) -> DepartureQuery {
    DepartureQuery::new(
        TransitServiceId::new(format!("tra:{train_no}")),
        BoardingPointId::new("stop-a"),
        after(),
        1,
    )
}

fn dispatcher(network: Arc<TransitNetwork>, tdx: Arc<TdxClient>) -> DepartureDispatcher {
    let tra = Arc::new(
        TraDepartureSource::new(network.clone())
            .with_tdx(Some(tdx))
            .with_realtime_endpoint("v3/Rail/TRA/TrainLiveBoard"),
    );
    DepartureDispatcher::new(network, vec![tra as Arc<dyn TransitDepartureSource>])
}

#[tokio::test]
async fn fresh_tra_realtime_overrides_static_timing_with_delay_adjusted_schedule() {
    let update = recent_update();
    let (tdx, _) = fixture_client(
        payload(vec![train("100", 7, &update)], &update),
        StatusCode::OK,
    )
    .await;
    let resolver = dispatcher(network_for(&["100"]), tdx);

    let result = resolver.next_departures(query("100")).await.unwrap();
    let trip = &result[0];

    assert_eq!(trip.timing_basis, TimingBasis::DelayAdjustedSchedule);
    assert_eq!(trip.timing_reliability, TimingReliability::Reliable);
    assert_eq!(trip.stop_times[0].departure_at, expected_departure(7));
    assert_eq!(trip.stop_times[1].arrival_at, expected_departure(17));
    assert_eq!(
        trip.scheduled_trip_id,
        Some(ScheduledTripId::new("scheduled-100"))
    );
    assert!(trip.observed_at.is_some());
}

#[tokio::test]
async fn empty_tra_realtime_falls_back_to_static_schedule() {
    let update = recent_update();
    let (tdx, _) = fixture_client(payload(Vec::new(), &update), StatusCode::OK).await;
    let resolver = dispatcher(network_for(&["100"]), tdx);

    let trip = &resolver.next_departures(query("100")).await.unwrap()[0];

    assert_eq!(trip.timing_basis, TimingBasis::Schedule);
    assert_eq!(trip.timing_reliability, TimingReliability::Reliable);
    assert_eq!(trip.stop_times[0].departure_at, expected_departure(0));
}

#[tokio::test]
async fn stale_tra_realtime_falls_back_to_static_schedule() {
    let update = (OffsetDateTime::now_utc() - Duration::minutes(10))
        .format(&Rfc3339)
        .unwrap();
    let (tdx, _) = fixture_client(
        payload(vec![train("100", 7, &update)], &update),
        StatusCode::OK,
    )
    .await;
    let resolver = dispatcher(network_for(&["100"]), tdx);

    let trip = &resolver.next_departures(query("100")).await.unwrap()[0];

    assert_eq!(trip.timing_basis, TimingBasis::Schedule);
    assert_eq!(trip.stop_times[0].departure_at, expected_departure(0));
}

#[tokio::test]
async fn malformed_and_failed_tra_realtime_fall_back_safely() {
    let (malformed_tdx, _) =
        fixture_client(json!({"TrainLiveBoards": "not-an-array"}), StatusCode::OK).await;
    let malformed = dispatcher(network_for(&["100"]), malformed_tdx)
        .next_departures(query("100"))
        .await
        .unwrap();
    assert_eq!(malformed[0].timing_basis, TimingBasis::Schedule);

    let (failed_tdx, _) = fixture_client(Value::Null, StatusCode::INTERNAL_SERVER_ERROR).await;
    let failed = dispatcher(network_for(&["100"]), failed_tdx)
        .next_departures(query("100"))
        .await
        .unwrap();
    assert_eq!(failed[0].timing_basis, TimingBasis::Schedule);
}

#[tokio::test]
async fn train_number_matching_does_not_cross_match_similar_numbers() {
    let update = recent_update();
    let (tdx, stats) = fixture_client(
        payload(vec![train("1000", 99, &update)], &update),
        StatusCode::OK,
    )
    .await;
    let resolver = dispatcher(network_for(&["100"]), tdx);

    let trip = &resolver.next_departures(query("100")).await.unwrap()[0];

    assert_eq!(trip.timing_basis, TimingBasis::Schedule);
    assert_eq!(trip.stop_times[0].departure_at, expected_departure(0));
    assert!(stats.requests.lock().unwrap()[0].contains("100"));
    assert!(!stats.requests.lock().unwrap()[0].contains("1000"));
}

#[tokio::test]
async fn duplicate_train_queries_are_served_by_one_tdx_request() {
    let update = recent_update();
    let (tdx, stats) = fixture_client(
        payload(
            vec![train("100", 3, &update), train("101", 5, &update)],
            &update,
        ),
        StatusCode::OK,
    )
    .await;
    let network = network_for(&["100", "101"]);
    let resolver = dispatcher(network, tdx);

    let results = resolver
        .next_departures_batch(vec![query("100"), query("100"), query("101")])
        .await;

    assert_eq!(stats.calls.load(Ordering::Relaxed), 1);
    assert!(results.iter().all(|result| {
        result
            .as_ref()
            .is_ok_and(|trips| trips[0].timing_basis == TimingBasis::DelayAdjustedSchedule)
    }));
    assert_eq!(
        results[0].as_ref().unwrap()[0].stop_times[0].departure_at,
        expected_departure(3)
    );
    assert_eq!(
        results[2].as_ref().unwrap()[0].stop_times[0].departure_at,
        expected_departure(5)
    );
}

#[tokio::test]
async fn tra_static_routing_still_works_without_tdx() {
    let network = network_for(&["100"]);
    let source = TraDepartureSource::new(network.clone());
    let resolver = DepartureDispatcher::new(
        network,
        vec![Arc::new(source) as Arc<dyn TransitDepartureSource>],
    );

    let result = resolver.next_departures(query("100")).await.unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Reliable);
}
