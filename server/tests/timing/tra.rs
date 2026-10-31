use std::sync::{
    Arc, Mutex,
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
use time::{
    Duration, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset, Weekday,
    format_description::well_known::Rfc3339,
};
use via_server::{
    domain::{
        ids::{
            BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
        },
        realtime::FreshnessState,
        transit::{
            ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime,
            ServiceCalendar, TransitMode, TransitNetwork, TransitPattern, TransitRoute,
            TransitTrip,
        },
        transit_runtime::TransitRuntime,
        transit_timing::{DepartureQuery, TimingBasis, TimingReliability},
    },
    providers::timing::{
        DepartureResolver, TdxProviderBindings, TraDepartureSource, TraTripBinding,
        TransitDepartureSource,
    },
    tdx::TdxClient,
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
    (spawn_tdx_fixture(app).await, stats)
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

fn network_for(train_numbers: &[&str]) -> Arc<TransitRuntime> {
    let board = BoardingPointId::new("stop-a");
    let alight = BoardingPointId::new("stop-b");
    let patterns = train_numbers
        .iter()
        .map(|train_no| {
            TransitPattern::new(
                TransitPatternId::new(format!("tra:{train_no}")),
                TransitRouteId::new("tra"),
                None,
                "Taipei",
                vec![board.clone(), alight.clone()],
                SegmentProfile {
                    segments: vec![SegmentTravelTime {
                        seconds: 600,
                        source: SegmentTimeSource::Schedule,
                    }],
                },
            )
        })
        .collect();
    let trips = train_numbers
        .iter()
        .map(|train_no| {
            TransitTrip::new(
                TransitTripId::new(format!("scheduled-{train_no}")),
                TransitPatternId::new(format!("tra:{train_no}")),
                ServiceCalendarId::new("daily"),
                vec![
                    ScheduledStopTime::new(36_000, 36_000),
                    ScheduledStopTime::new(36_600, 36_600),
                ],
            )
        })
        .collect();
    Arc::new(TransitRuntime::new(Arc::new(
        TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            patterns,
            trips,
        )
        .with_routes(vec![TransitRoute {
            id: TransitRouteId::new("tra"),
            mode: TransitMode::Rail,
            label: "TRA".into(),
        }])
        .with_calendars(vec![ServiceCalendar {
            id: ServiceCalendarId::new("daily"),
            weekdays: vec![
                Weekday::Monday,
                Weekday::Tuesday,
                Weekday::Wednesday,
                Weekday::Thursday,
                Weekday::Friday,
                Weekday::Saturday,
                Weekday::Sunday,
            ],
            start_date: None,
            end_date: None,
            added_dates: Vec::new(),
            removed_dates: Vec::new(),
        }])
        .with_revisions("static", "schedule"),
    )))
}

fn query(train_no: &str) -> DepartureQuery {
    DepartureQuery::new(
        TransitPatternId::new(format!("tra:{train_no}")),
        BoardingPointId::new("stop-a"),
        after(),
        1,
    )
}

fn departure_resolver(network: Arc<TransitRuntime>, tdx: Arc<TdxClient>) -> DepartureResolver {
    let tra = Arc::new(
        TraDepartureSource::new(network.clone(), tdx)
            .with_bindings(Arc::new(TdxProviderBindings {
                tra_trips: network
                    .network()
                    .trips
                    .iter()
                    .map(|trip| {
                        (
                            trip.id.clone(),
                            TraTripBinding {
                                train_number: trip
                                    .id
                                    .as_str()
                                    .trim_start_matches("scheduled-")
                                    .into(),
                            },
                        )
                    })
                    .collect(),
                ..Default::default()
            }))
            .with_realtime_endpoint("v3/Rail/TRA/TrainLiveBoard"),
    );
    DepartureResolver::new(network, vec![tra as Arc<dyn TransitDepartureSource>])
}

#[tokio::test]
async fn fresh_tra_realtime_overrides_static_timing_with_delay_adjusted_schedule() {
    let update = recent_update();
    let (tdx, _) = fixture_client(
        payload(vec![train("100", 7, &update)], &update),
        StatusCode::OK,
    )
    .await;
    let resolver = departure_resolver(network_for(&["100"]), tdx);

    let result = resolver.next_departures(query("100")).await.unwrap();
    let trip = &result[0];

    assert_eq!(trip.timing_basis, TimingBasis::DelayAdjustedSchedule);
    assert_eq!(trip.timing_reliability, TimingReliability::Reliable);
    assert_eq!(trip.stop_times[0].departure_at, expected_departure(7));
    assert_eq!(trip.stop_times[1].arrival_at, expected_departure(17));
    assert_eq!(
        trip.scheduled_trip_id,
        Some(TransitTripId::new("scheduled-100"))
    );
    assert!(trip.observed_at.is_some());
    assert_eq!(trip.evidence.len(), 1);
    assert_eq!(trip.evidence[0].freshness, FreshnessState::Fresh);
    assert_eq!(
        trip.evidence[0].value.predicted_arrival_at,
        Some(expected_departure(7))
    );
    assert_eq!(
        trip.evidence[0].value.boarding_point_id,
        query("100").board_stop_id
    );
    assert_eq!(
        Some(trip.evidence[0].value.transmitted_at),
        trip.observed_at
    );
}

#[tokio::test]
async fn empty_tra_realtime_falls_back_to_static_schedule() {
    let update = recent_update();
    let (tdx, _) = fixture_client(payload(Vec::new(), &update), StatusCode::OK).await;
    let resolver = departure_resolver(network_for(&["100"]), tdx);

    let trip = &resolver.next_departures(query("100")).await.unwrap()[0];

    assert_eq!(trip.timing_basis, TimingBasis::Schedule);
    assert_eq!(trip.timing_reliability, TimingReliability::Reliable);
    assert_eq!(trip.stop_times[0].departure_at, expected_departure(0));
    assert!(trip.evidence.is_empty());
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
    let resolver = departure_resolver(network_for(&["100"]), tdx);

    let trip = &resolver.next_departures(query("100")).await.unwrap()[0];

    assert_eq!(trip.timing_basis, TimingBasis::Schedule);
    assert_eq!(trip.stop_times[0].departure_at, expected_departure(0));
}

#[tokio::test]
async fn malformed_and_failed_tra_realtime_fall_back_safely() {
    let (malformed_tdx, _) =
        fixture_client(json!({"TrainLiveBoards": "not-an-array"}), StatusCode::OK).await;
    let malformed = departure_resolver(network_for(&["100"]), malformed_tdx)
        .next_departures(query("100"))
        .await
        .unwrap();
    assert_eq!(malformed[0].timing_basis, TimingBasis::Schedule);

    let (failed_tdx, _) = fixture_client(Value::Null, StatusCode::INTERNAL_SERVER_ERROR).await;
    let failed = departure_resolver(network_for(&["100"]), failed_tdx)
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
    let resolver = departure_resolver(network_for(&["100"]), tdx);

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
    let resolver = departure_resolver(network, tdx);

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
async fn tra_static_routing_falls_back_when_realtime_is_unavailable() {
    let network = network_for(&["100"]);
    let source =
        TraDepartureSource::new(network.clone(), crate::support::tdx::failing_tdx_client());
    let resolver = DepartureResolver::new(
        network,
        vec![Arc::new(source) as Arc<dyn TransitDepartureSource>],
    );

    let result = resolver.next_departures(query("100")).await.unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Reliable);
}
