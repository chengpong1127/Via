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
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339, macros::datetime};
use via_server::{
    domain::{
        ids::{
            BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
        },
        transit::{
            ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime,
            ServiceCalendar, TransitMode, TransitNetwork, TransitPattern, TransitRoute,
            TransitTrip,
        },
        transit_runtime::TransitRuntime,
        transit_timing::{DepartureQuery, TimingBasis, TimingReliability},
    },
    providers::timing::{
        BusDepartureSource, BusPatternBinding, DepartureResolver, TdxProviderBindings,
        TransitDepartureSource,
    },
    tdx::TdxClient,
};

const NOW: OffsetDateTime = datetime!(2026-08-31 10:00 +08:00);

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

async fn scoped_fixture_client(
    payload: Value,
    failing_scope: &'static str,
) -> (Arc<TdxClient>, FixtureStats) {
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
            if uri.path().contains(failing_scope) {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            }
            (StatusCode::OK, Json(payload)).into_response()
        }
    }));
    (spawn_tdx_fixture(app).await, stats)
}

async fn rate_limit_after_first_client(payload: Value) -> (Arc<TdxClient>, FixtureStats) {
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
                    Json(json!({"access_token": "fixture-token", "expires_in": 300})),
                )
                    .into_response();
            }
            let call = calls.fetch_add(1, Ordering::Relaxed);
            requests.lock().unwrap().push(uri.to_string());
            if call == 0 {
                (StatusCode::OK, Json(payload)).into_response()
            } else {
                StatusCode::TOO_MANY_REQUESTS.into_response()
            }
        }
    }));
    (spawn_tdx_fixture(app).await, stats)
}

fn recent_update() -> String {
    (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap()
}

fn arrival(
    pattern_id: &str,
    board_stop_id: &str,
    estimate_seconds: i64,
    update_time: &str,
) -> Value {
    json!({
        "StopUID": board_stop_id,
        "SubRouteUID": pattern_id.split_once("::direction=").map_or(pattern_id, |(id, _)| id),
        "Direction": pattern_id.split_once("::direction=").and_then(|(_, direction)| direction.parse::<u8>().ok()),
        "StopStatus": 0,
        "EstimateTime": estimate_seconds,
        "UpdateTime": update_time
    })
}

fn calendar() -> ServiceCalendar {
    ServiceCalendar {
        id: ServiceCalendarId::new("daily"),
        weekdays: vec![time::Weekday::Monday],
        start_date: None,
        end_date: None,
        added_dates: Vec::new(),
        removed_dates: Vec::new(),
    }
}

fn bus_pattern(
    id: TransitPatternId,
    board: BoardingPointId,
    downstream: BoardingPointId,
) -> TransitPattern {
    TransitPattern::new(
        id,
        TransitRouteId::new("bus"),
        None,
        "Downstream",
        vec![board, downstream],
        SegmentProfile {
            segments: vec![SegmentTravelTime {
                seconds: 120,
                source: SegmentTimeSource::Estimated,
            }],
        },
    )
}

fn trip(id: TransitTripId, pattern_id: TransitPatternId) -> TransitTrip {
    TransitTrip::new(
        id,
        pattern_id,
        ServiceCalendarId::new("daily"),
        vec![
            ScheduledStopTime::new(10 * 3600, 10 * 3600),
            ScheduledStopTime::new(10 * 3600 + 600, 10 * 3600 + 600),
        ],
    )
}

fn network_for(pattern_ids: &[TransitPatternId]) -> TransitNetwork {
    network_for_with_scopes(pattern_ids, &[])
}

fn runtime(network: TransitNetwork) -> Arc<TransitRuntime> {
    Arc::new(TransitRuntime::new(Arc::new(network)))
}

fn network_for_with_scopes(
    pattern_ids: &[TransitPatternId],
    _scopes: &[(&str, &str)],
) -> TransitNetwork {
    let board = BoardingPointId::new("stop-x");
    let downstream = BoardingPointId::new("stop-y");
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        pattern_ids
            .iter()
            .map(|id| bus_pattern(id.clone(), board.clone(), downstream.clone()))
            .collect(),
        pattern_ids
            .iter()
            .enumerate()
            .map(|(index, id)| trip(TransitTripId::new(format!("run-{index}")), id.clone()))
            .collect(),
    )
    .with_routes(vec![TransitRoute {
        id: TransitRouteId::new("bus"),
        mode: TransitMode::Bus,
        label: "Bus".into(),
    }])
    .with_calendars(vec![calendar()])
    .with_revisions("static", "schedule")
}

fn network_for_unique_stops(count: usize) -> (TransitNetwork, Vec<DepartureQuery>) {
    let mut patterns = Vec::with_capacity(count);
    let mut trips = Vec::with_capacity(count);
    let mut queries = Vec::with_capacity(count);
    for index in 0..count {
        let id = TransitPatternId::new(format!("service-{index}"));
        let board = BoardingPointId::new(format!("stop-{index}"));
        patterns.push(bus_pattern(
            id.clone(),
            board.clone(),
            BoardingPointId::new(format!("downstream-{index}")),
        ));
        trips.push(trip(TransitTripId::new(format!("run-{index}")), id.clone()));
        queries.push(DepartureQuery::new(id, board, NOW, 1));
    }
    (
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            patterns,
            trips,
        )
        .with_routes(vec![TransitRoute {
            id: TransitRouteId::new("bus"),
            mode: TransitMode::Bus,
            label: "Bus".into(),
        }])
        .with_calendars(vec![calendar()])
        .with_revisions("static", "schedule"),
        queries,
    )
}

fn bindings(network: &TransitNetwork, scopes: &[(&str, &str)]) -> Arc<TdxProviderBindings> {
    let bus_patterns = network
        .patterns
        .iter()
        .map(|pattern| {
            let scope = scopes
                .iter()
                .find(|(id, _)| *id == pattern.id.as_str())
                .map_or("Taipei", |(_, scope)| *scope);
            (
                pattern.id.clone(),
                BusPatternBinding {
                    tdx_service_id: pattern.id.as_str().into(),
                    realtime_scope: scope.into(),
                },
            )
        })
        .collect();
    Arc::new(TdxProviderBindings {
        bus_patterns,
        ..Default::default()
    })
}

fn query(pattern_id: &str, board_stop_id: &str, after: OffsetDateTime) -> DepartureQuery {
    DepartureQuery::new(
        TransitPatternId::new(pattern_id),
        BoardingPointId::new(board_stop_id),
        after,
        3,
    )
}

fn bus_resolver(network: Arc<TransitRuntime>, tdx: Arc<TdxClient>) -> DepartureResolver {
    let bus = Arc::new(
        BusDepartureSource::new(network.clone(), Some(tdx))
            .with_bindings(bindings(network.network(), &[]))
            .with_realtime_endpoint("feed"),
    );
    DepartureResolver::new(network, vec![bus as Arc<dyn TransitDepartureSource>])
}

#[tokio::test]
async fn twenty_logical_bus_needs_use_one_direct_tdx_batch() {
    let pattern_ids = (0..20)
        .map(|index| TransitPatternId::new(format!("service-{index}")))
        .collect::<Vec<_>>();
    let update = recent_update();
    let payload = Value::Array(
        pattern_ids
            .iter()
            .map(|pattern_id| arrival(pattern_id.as_str(), "stop-x", 60, &update))
            .collect(),
    );
    let (tdx, stats) = fixture_client(payload, StatusCode::OK).await;
    let network = runtime(network_for(&pattern_ids));
    let resolver = bus_resolver(network, tdx);
    let needs = pattern_ids
        .iter()
        .map(|pattern_id| {
            DepartureQuery::new(pattern_id.clone(), BoardingPointId::new("stop-x"), NOW, 1)
        })
        .collect::<Vec<_>>();

    let results = resolver.next_departures_batch(needs).await;

    assert!(
        results
            .iter()
            .all(|result| result.as_ref().is_ok_and(|trips| !trips.is_empty()))
    );
    assert_eq!(stats.calls.load(Ordering::Relaxed), 1);
    assert!(stats.requests.lock().unwrap()[0].contains("filter="));
    assert_eq!(
        results[0].as_ref().unwrap()[0].timing_reliability,
        TimingReliability::Reliable
    );
}

#[tokio::test]
async fn empty_bus_realtime_falls_back_to_static_schedule() {
    let pattern_id = TransitPatternId::new("service-a");
    let (tdx, _) = fixture_client(json!([]), StatusCode::OK).await;
    let network = runtime(network_for(std::slice::from_ref(&pattern_id)));
    let resolver = bus_resolver(network, tdx);

    let result = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap();

    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Fallback);
}

#[tokio::test]
async fn unscoped_bus_service_skips_tdx_and_falls_back_to_static_schedule() {
    let pattern_id = TransitPatternId::new("service-without-scope");
    let (tdx, stats) = fixture_client(json!([]), StatusCode::OK).await;
    let network = runtime(network_for(std::slice::from_ref(&pattern_id)));
    let source = BusDepartureSource::new(network.clone(), Some(tdx.clone()));

    let realtime = source
        .realtime_departures_batch(vec![query("service-without-scope", "stop-x", NOW)])
        .await
        .unwrap();
    assert!(realtime[0].as_ref().is_ok_and(Vec::is_empty));
    assert_eq!(stats.calls.load(Ordering::Relaxed), 0);

    let resolver = bus_resolver(network, tdx);
    let departures = resolver
        .next_departures(query("service-without-scope", "stop-x", NOW))
        .await
        .unwrap();
    assert_eq!(departures[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(
        departures[0].timing_reliability,
        TimingReliability::Fallback
    );
}

#[tokio::test]
async fn bus_batch_keeps_realtime_and_static_reliability_per_query() {
    let pattern_ids = [
        TransitPatternId::new("service-a"),
        TransitPatternId::new("service-b"),
    ];
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let resolver = bus_resolver(runtime(network_for(&pattern_ids)), tdx);

    let results = resolver
        .next_departures_batch(vec![
            query("service-a", "stop-x", NOW),
            query("service-b", "stop-x", NOW),
        ])
        .await;

    assert_eq!(
        results[0].as_ref().unwrap()[0].timing_reliability,
        TimingReliability::Reliable
    );
    assert_eq!(
        results[1].as_ref().unwrap()[0].timing_reliability,
        TimingReliability::Fallback
    );
}

#[tokio::test]
async fn failed_bus_batch_only_falls_back_for_its_scope() {
    let pattern_ids = [
        TransitPatternId::new("service-taipei"),
        TransitPatternId::new("service-new-taipei"),
    ];
    let scopes = [
        ("service-taipei", "Taipei"),
        ("service-new-taipei", "NewTaipei"),
    ];
    let update = recent_update();
    let (tdx, stats) = scoped_fixture_client(
        json!([
            arrival("service-taipei", "stop-x", 60, &update),
            arrival("service-new-taipei", "stop-x", 120, &update)
        ]),
        "NewTaipei",
    )
    .await;
    let network = runtime(network_for_with_scopes(&pattern_ids, &scopes));
    let bus = Arc::new(
        BusDepartureSource::new(network.clone(), Some(tdx))
            .with_bindings(bindings(network.network(), &scopes)),
    );
    let resolver = DepartureResolver::new(network, vec![bus as Arc<dyn TransitDepartureSource>]);

    let results = resolver
        .next_departures_batch(vec![
            query("service-taipei", "stop-x", NOW),
            query("service-new-taipei", "stop-x", NOW),
        ])
        .await;

    assert_eq!(stats.calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        results[0].as_ref().unwrap()[0].timing_reliability,
        TimingReliability::Reliable
    );
    assert_eq!(
        results[1].as_ref().unwrap()[0].timing_reliability,
        TimingReliability::Fallback
    );
}

#[tokio::test]
async fn realtime_effective_trip_uses_indexed_static_shape_without_identity() {
    let pattern_id = TransitPatternId::new("service-a");
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let resolver = bus_resolver(runtime(network_for(std::slice::from_ref(&pattern_id))), tdx);

    let trip = &resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap()[0];
    assert_eq!(trip.scheduled_trip_id, None);
    assert!(matches!(trip.timing_basis, TimingBasis::RealtimePrediction));
    assert!(trip.stop_times[1].arrival_at > trip.stop_times[0].departure_at);
    assert!(
        trip.stop_times
            .windows(2)
            .all(|times| times[0].departure_at <= times[1].arrival_at)
    );
}

#[tokio::test]
async fn realtime_effective_trip_uses_estimated_segments_without_static_shape() {
    let pattern_id = TransitPatternId::new("service-a");
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let mut network = network_for(std::slice::from_ref(&pattern_id));
    network.trips.clear();
    let resolver = bus_resolver(runtime(network), tdx);

    let trip = &resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap()[0];
    assert_eq!(trip.scheduled_trip_id, None);
    assert_eq!(
        trip.stop_times[1].arrival_at - trip.stop_times[0].departure_at,
        Duration::seconds(120)
    );
}

#[tokio::test]
async fn realtime_bus_uses_derived_downstream_segment_without_a_fabricated_schedule() {
    let pattern_id = TransitPatternId::new("service-a");
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let mut network = network_for(std::slice::from_ref(&pattern_id));
    network.trips.clear();
    network.patterns[0].static_profile.segments[0] = SegmentTravelTime {
        seconds: 347,
        source: SegmentTimeSource::Derived,
    };
    let resolver = bus_resolver(runtime(network), tdx);

    let trip = &resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap()[0];
    assert_eq!(trip.scheduled_trip_id, None);
    assert_eq!(trip.timing_basis, TimingBasis::RealtimePrediction);
    assert_eq!(
        trip.stop_times[1].arrival_at - trip.stop_times[0].departure_at,
        Duration::seconds(347)
    );
}

#[tokio::test]
async fn realtime_bus_uses_position_aligned_profile_on_a_loop() {
    let pattern_id = TransitPatternId::new("service-a");
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let mut network = network_for(std::slice::from_ref(&pattern_id));
    network.trips.clear();
    network.patterns[0].stops = vec![
        BoardingPointId::new("stop-x"),
        BoardingPointId::new("stop-y"),
        BoardingPointId::new("stop-x"),
        BoardingPointId::new("stop-z"),
    ];
    network.patterns[0].static_profile.segments = [120, 180, 347]
        .into_iter()
        .map(|seconds| SegmentTravelTime {
            seconds,
            source: SegmentTimeSource::Derived,
        })
        .collect();
    let resolver = bus_resolver(runtime(network), tdx);

    let trip = &resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap()[0];
    assert_eq!(
        trip.stop_times[3].arrival_at - trip.stop_times[0].departure_at,
        Duration::seconds(647)
    );
}

#[tokio::test]
async fn malformed_or_stale_realtime_falls_back_safely() {
    let pattern_id = TransitPatternId::new("service-a");
    let stale = (OffsetDateTime::now_utc() - Duration::seconds(400))
        .format(&Rfc3339)
        .unwrap();
    let (tdx, _) = fixture_client(
        json!([
            {"StopUID": "stop-x", "SubRouteUID": "service-a", "StopStatus": 0, "EstimateTime": "bad", "UpdateTime": stale}
        ]),
        StatusCode::OK,
    )
    .await;
    let resolver = bus_resolver(runtime(network_for(std::slice::from_ref(&pattern_id))), tdx);
    let departures = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap();
    assert_eq!(departures[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(
        departures[0].timing_reliability,
        TimingReliability::Fallback
    );
}

#[tokio::test]
async fn bus_needs_are_deduplicated_by_stop_and_grouped_by_realtime_scope() {
    let pattern_ids = (0..97)
        .map(|index| TransitPatternId::new(format!("service-{index}")))
        .collect::<Vec<_>>();
    let scopes = pattern_ids
        .iter()
        .enumerate()
        .map(|(index, pattern_id)| {
            (
                pattern_id.as_str(),
                if index < 65 { "Taipei" } else { "NewTaipei" },
            )
        })
        .collect::<Vec<_>>();
    let update = recent_update();
    let payload = Value::Array(
        pattern_ids
            .iter()
            .map(|pattern_id| arrival(pattern_id.as_str(), "stop-x", 60, &update))
            .collect(),
    );
    let (tdx, stats) = fixture_client(payload, StatusCode::OK).await;
    let network = runtime(network_for_with_scopes(&pattern_ids, &scopes));
    let bus = Arc::new(
        BusDepartureSource::new(network.clone(), Some(tdx))
            .with_bindings(bindings(network.network(), &scopes)),
    );
    let resolver = DepartureResolver::new(network, vec![bus as Arc<dyn TransitDepartureSource>]);
    let needs = pattern_ids
        .iter()
        .map(|pattern_id| {
            DepartureQuery::new(pattern_id.clone(), BoardingPointId::new("stop-x"), NOW, 1)
        })
        .collect::<Vec<_>>();

    let results = resolver.next_departures_batch(needs).await;

    assert!(
        results
            .iter()
            .all(|result| result.as_ref().is_ok_and(|trips| !trips.is_empty()))
    );
    assert_eq!(stats.calls.load(Ordering::Relaxed), 2);
    let requests = stats.requests.lock().unwrap();
    let mut scopes_seen = Vec::new();
    for request in requests.iter() {
        assert!(request.contains("StopUID"));
        assert!(!request.contains("SubRouteUID"));
        assert_eq!(request.matches("stop-x").count(), 1);
        let scope = if request.contains("/NewTaipei?") {
            "NewTaipei"
        } else {
            "Taipei"
        };
        scopes_seen.push(scope);
    }
    scopes_seen.sort_unstable();
    assert_eq!(scopes_seen, vec!["NewTaipei", "Taipei"]);
}

#[tokio::test]
async fn duplicate_pairs_are_fetched_once_and_results_are_partitioned_exactly() {
    let pattern_ids = [
        TransitPatternId::new("service-a"),
        TransitPatternId::new("service-b"),
    ];
    let update = recent_update();
    let (tdx, stats) = fixture_client(
        json!([
            arrival("service-a", "stop-x", 60, &update),
            arrival("service-b", "stop-y", 120, &update),
            arrival("service-a", "stop-y", 180, &update),
            arrival("service-b", "stop-x", 240, &update)
        ]),
        StatusCode::OK,
    )
    .await;
    let resolver = bus_resolver(runtime(network_for(&pattern_ids)), tdx);
    let second_after = OffsetDateTime::now_utc() + Duration::seconds(90);
    let results = resolver
        .next_departures_batch(vec![
            query("service-a", "stop-x", NOW),
            query("service-a", "stop-x", second_after),
            query("service-b", "stop-x", NOW),
        ])
        .await;

    assert_eq!(stats.calls.load(Ordering::Relaxed), 1);
    assert_eq!(results[0].as_ref().unwrap().len(), 1);
    assert!(results[1].as_ref().unwrap().is_empty());
    assert_eq!(results[2].as_ref().unwrap().len(), 1);
    assert_eq!(results[2].as_ref().unwrap()[0].pattern_id, pattern_ids[1]);
}

#[tokio::test]
async fn directional_patterns_do_not_cross_satisfy_each_other() {
    let direction_zero = "route::direction=0";
    let direction_one = "route::direction=1";
    let pattern_ids = [
        TransitPatternId::new(direction_zero),
        TransitPatternId::new(direction_one),
    ];
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([
            arrival(direction_zero, "stop-x", 60, &update),
            arrival(direction_one, "stop-x", 120, &update)
        ]),
        StatusCode::OK,
    )
    .await;
    let resolver = bus_resolver(runtime(network_for(&pattern_ids)), tdx);
    let results = resolver
        .next_departures_batch(vec![
            query(direction_zero, "stop-x", NOW),
            query(direction_one, "stop-x", NOW),
        ])
        .await;

    assert_eq!(
        results[0].as_ref().unwrap()[0].pattern_id.as_str(),
        direction_zero
    );
    assert_eq!(
        results[1].as_ref().unwrap()[0].pattern_id.as_str(),
        direction_one
    );
}

#[tokio::test]
async fn rate_limited_bus_realtime_falls_back_to_static_schedule() {
    let pattern_id = TransitPatternId::new("service-a");
    let (tdx, _) = fixture_client(json!([]), StatusCode::TOO_MANY_REQUESTS).await;
    let resolver = bus_resolver(runtime(network_for(std::slice::from_ref(&pattern_id))), tdx);

    let result = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Fallback);
}

#[tokio::test]
async fn first_rate_limit_stops_remaining_chunks_for_the_scope() {
    let (network, queries) = network_for_unique_stops(201);
    let (tdx, stats) = fixture_client(json!([]), StatusCode::TOO_MANY_REQUESTS).await;
    let resolver = bus_resolver(runtime(network), tdx);

    let results = resolver.next_departures_batch(queries).await;

    assert_eq!(stats.calls.load(Ordering::Relaxed), 1);
    assert!(results.iter().all(|result| {
        result.as_ref().is_ok_and(|trips| {
            trips.len() == 1 && trips[0].timing_reliability == TimingReliability::Fallback
        })
    }));
}

#[tokio::test]
async fn later_rate_limit_preserves_successful_earlier_chunks() {
    let (network, queries) = network_for_unique_stops(201);
    let update = recent_update();
    let payload = Value::Array(
        (0..201)
            .map(|index| {
                arrival(
                    &format!("service-{index}"),
                    &format!("stop-{index}"),
                    60,
                    &update,
                )
            })
            .collect(),
    );
    let (tdx, stats) = rate_limit_after_first_client(payload).await;
    let resolver = bus_resolver(runtime(network), tdx);

    let results = resolver.next_departures_batch(queries).await;

    assert_eq!(stats.calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        results
            .iter()
            .filter(|result| {
                result
                    .as_ref()
                    .is_ok_and(|trips| trips[0].timing_reliability == TimingReliability::Reliable)
            })
            .count(),
        100
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| {
                result
                    .as_ref()
                    .is_ok_and(|trips| trips[0].timing_reliability == TimingReliability::Fallback)
            })
            .count(),
        101
    );
}
