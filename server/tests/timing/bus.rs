use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
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
        ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
        transit::{ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService},
        transit_timing::{DepartureQuery, DepartureResolver, TimingBasis, TimingReliability},
    },
    providers::timing::{BusDepartureSource, DepartureDispatcher, TransitDepartureSource},
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

fn recent_update() -> String {
    (OffsetDateTime::now_utc() - Duration::seconds(30))
        .format(&Rfc3339)
        .unwrap()
}

fn arrival(
    service_id: &str,
    board_stop_id: &str,
    estimate_seconds: i64,
    update_time: &str,
) -> Value {
    json!({
        "StopUID": board_stop_id,
        "SubRouteUID": service_id.split_once("::direction=").map_or(service_id, |(id, _)| id),
        "Direction": service_id.split_once("::direction=").and_then(|(_, direction)| direction.parse::<u8>().ok()),
        "StopStatus": 0,
        "EstimateTime": estimate_seconds,
        "UpdateTime": update_time
    })
}

fn network_for(service_ids: &[TransitServiceId]) -> TransitNetwork {
    network_for_with_scopes(service_ids, &[])
}

fn network_for_with_scopes(
    service_ids: &[TransitServiceId],
    scopes: &[(&str, &str)],
) -> TransitNetwork {
    let board = BoardingPointId::new("stop-x");
    let downstream = BoardingPointId::new("stop-y");
    let services = service_ids
        .iter()
        .map(|service_id| {
            let service = TransitService::new(
                service_id.clone(),
                TransitMode::Bus,
                "Bus",
                "Downstream",
                vec![board.clone(), downstream.clone()],
            )
            .with_timing_source("bus_n1");
            scopes
                .iter()
                .find_map(|(id, scope)| (*id == service_id.as_str()).then_some(*scope))
                .map_or(service.clone(), |scope| service.with_realtime_scope(scope))
        })
        .collect::<Vec<_>>();
    let trips = service_ids
        .iter()
        .enumerate()
        .map(|(index, service_id)| {
            ScheduledTrip::weekdays(
                ScheduledTripId::new(format!("run-{index}")),
                service_id.clone(),
                vec![time::Weekday::Monday],
                vec![
                    ScheduledStopTime::new(10 * 3600, 10 * 3600),
                    ScheduledStopTime::new(10 * 3600 + 600, 10 * 3600 + 600),
                ],
            )
        })
        .collect::<Vec<_>>();
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        services,
        trips,
    )
    .with_revisions("static", "schedule")
}

fn query(service_id: &str, board_stop_id: &str, after: OffsetDateTime) -> DepartureQuery {
    DepartureQuery::new(
        TransitServiceId::new(service_id),
        BoardingPointId::new(board_stop_id),
        after,
        3,
    )
}

fn bus_dispatcher(network: Arc<TransitNetwork>, tdx: Arc<TdxClient>) -> DepartureDispatcher {
    let bus = Arc::new(
        BusDepartureSource::new(network.clone(), Some(tdx)).with_realtime_endpoint("feed"),
    );
    DepartureDispatcher::new(network, vec![bus as Arc<dyn TransitDepartureSource>])
}

#[tokio::test]
async fn twenty_logical_bus_needs_use_one_direct_tdx_batch() {
    let service_ids = (0..20)
        .map(|index| TransitServiceId::new(format!("service-{index}")))
        .collect::<Vec<_>>();
    let update = recent_update();
    let payload = Value::Array(
        service_ids
            .iter()
            .map(|service_id| arrival(service_id.as_str(), "stop-x", 60, &update))
            .collect(),
    );
    let (tdx, stats) = fixture_client(payload, StatusCode::OK).await;
    let network = Arc::new(network_for(&service_ids));
    let resolver = bus_dispatcher(network, tdx);
    let needs = service_ids
        .iter()
        .map(|service_id| {
            DepartureQuery::new(service_id.clone(), BoardingPointId::new("stop-x"), NOW, 1)
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
    let service_id = TransitServiceId::new("service-a");
    let (tdx, _) = fixture_client(json!([]), StatusCode::OK).await;
    let network = Arc::new(network_for(std::slice::from_ref(&service_id)));
    let resolver = bus_dispatcher(network, tdx);

    let result = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap();

    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Fallback);
}

#[tokio::test]
async fn bus_batch_keeps_realtime_and_static_reliability_per_query() {
    let service_ids = [
        TransitServiceId::new("service-a"),
        TransitServiceId::new("service-b"),
    ];
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let resolver = bus_dispatcher(Arc::new(network_for(&service_ids)), tdx);

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
    let service_ids = [
        TransitServiceId::new("service-taipei"),
        TransitServiceId::new("service-new-taipei"),
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
    let network = Arc::new(network_for_with_scopes(&service_ids, &scopes));
    let bus = Arc::new(BusDepartureSource::new(network.clone(), Some(tdx)));
    let resolver = DepartureDispatcher::new(network, vec![bus as Arc<dyn TransitDepartureSource>]);

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
    let service_id = TransitServiceId::new("service-a");
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let resolver = bus_dispatcher(
        Arc::new(network_for(std::slice::from_ref(&service_id))),
        tdx,
    );

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
    let service_id = TransitServiceId::new("service-a");
    let update = recent_update();
    let (tdx, _) = fixture_client(
        json!([arrival("service-a", "stop-x", 60, &update)]),
        StatusCode::OK,
    )
    .await;
    let mut network = network_for(std::slice::from_ref(&service_id));
    network.trips.clear();
    let resolver = bus_dispatcher(Arc::new(network), tdx);

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
async fn malformed_or_stale_realtime_falls_back_safely() {
    let service_id = TransitServiceId::new("service-a");
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
    let resolver = bus_dispatcher(
        Arc::new(network_for(std::slice::from_ref(&service_id))),
        tdx,
    );
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
async fn bus_needs_are_batched_per_realtime_scope_and_capped_at_64_pairs() {
    let service_ids = (0..97)
        .map(|index| TransitServiceId::new(format!("service-{index}")))
        .collect::<Vec<_>>();
    let scopes = service_ids
        .iter()
        .enumerate()
        .map(|(index, service_id)| {
            (
                service_id.as_str(),
                if index < 65 { "Taipei" } else { "NewTaipei" },
            )
        })
        .collect::<Vec<_>>();
    let update = recent_update();
    let payload = Value::Array(
        service_ids
            .iter()
            .map(|service_id| arrival(service_id.as_str(), "stop-x", 60, &update))
            .collect(),
    );
    let (tdx, stats) = fixture_client(payload, StatusCode::OK).await;
    let network = Arc::new(network_for_with_scopes(&service_ids, &scopes));
    let bus = Arc::new(BusDepartureSource::new(network.clone(), Some(tdx)));
    let resolver = DepartureDispatcher::new(network, vec![bus as Arc<dyn TransitDepartureSource>]);
    let needs = service_ids
        .iter()
        .map(|service_id| {
            DepartureQuery::new(service_id.clone(), BoardingPointId::new("stop-x"), NOW, 1)
        })
        .collect::<Vec<_>>();

    let results = resolver.next_departures_batch(needs).await;

    assert!(
        results
            .iter()
            .all(|result| result.as_ref().is_ok_and(|trips| !trips.is_empty()))
    );
    assert_eq!(stats.calls.load(Ordering::Relaxed), 3);
    let requests = stats.requests.lock().unwrap();
    let mut batch_sizes = HashMap::<&str, Vec<usize>>::new();
    for request in requests.iter() {
        let size = request.matches("service-").count();
        let scope = if request.contains("/NewTaipei?") {
            "NewTaipei"
        } else {
            "Taipei"
        };
        batch_sizes.entry(scope).or_default().push(size);
    }
    batch_sizes
        .values_mut()
        .for_each(|sizes| sizes.sort_unstable());
    assert_eq!(batch_sizes.get("Taipei"), Some(&vec![1, 64]));
    assert_eq!(batch_sizes.get("NewTaipei"), Some(&vec![32]));
}

#[tokio::test]
async fn duplicate_pairs_are_fetched_once_and_results_are_partitioned_exactly() {
    let service_ids = [
        TransitServiceId::new("service-a"),
        TransitServiceId::new("service-b"),
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
    let resolver = bus_dispatcher(Arc::new(network_for(&service_ids)), tdx);
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
    assert_eq!(results[2].as_ref().unwrap()[0].service_id, service_ids[1]);
}

#[tokio::test]
async fn directional_service_ids_do_not_cross_satisfy_each_other() {
    let direction_zero = "route::direction=0";
    let direction_one = "route::direction=1";
    let service_ids = [
        TransitServiceId::new(direction_zero),
        TransitServiceId::new(direction_one),
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
    let resolver = bus_dispatcher(Arc::new(network_for(&service_ids)), tdx);
    let results = resolver
        .next_departures_batch(vec![
            query(direction_zero, "stop-x", NOW),
            query(direction_one, "stop-x", NOW),
        ])
        .await;

    assert_eq!(
        results[0].as_ref().unwrap()[0].service_id.as_str(),
        direction_zero
    );
    assert_eq!(
        results[1].as_ref().unwrap()[0].service_id.as_str(),
        direction_one
    );
}

#[tokio::test]
async fn rate_limited_bus_realtime_falls_back_to_static_schedule() {
    let service_id = TransitServiceId::new("service-a");
    let (tdx, _) = fixture_client(json!([]), StatusCode::TOO_MANY_REQUESTS).await;
    let resolver = bus_dispatcher(
        Arc::new(network_for(std::slice::from_ref(&service_id))),
        tdx,
    );

    let result = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Fallback);
}
