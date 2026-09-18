use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use time::{Duration, OffsetDateTime, macros::datetime};
use via_server::{
    domain::{
        ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
        realtime::{
            ArrivalEstimate, ArrivalState, Freshness, FreshnessState, NormalizedRealtimeValue,
            RealtimeValue,
        },
        transit::{ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService},
        transit_timing::{DepartureQuery, DepartureResolver},
    },
    providers::{
        CompositeDepartureResolver,
        realtime::{ArrivalQuery, CachedRealtimeProvider, RealtimeError, RealtimeProvider},
        timing::RealtimeDepartureResolver,
    },
};

const NOW: OffsetDateTime = datetime!(2026-08-31 10:00 +08:00);

#[derive(Clone)]
struct BatchProvider {
    calls: Arc<AtomicUsize>,
    queries: Arc<Mutex<Vec<ArrivalQuery>>>,
    values: Arc<Vec<NormalizedRealtimeValue>>,
    delay: StdDuration,
    error: Option<RealtimeError>,
}

#[async_trait]
impl RealtimeProvider for BatchProvider {
    async fn arrival_estimates(
        &self,
        query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.queries.lock().unwrap().push(query);
        tokio::time::sleep(self.delay).await;
        self.error
            .clone()
            .map_or_else(|| Ok(self.values.as_ref().clone()), Err)
    }

    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        Ok(Vec::new())
    }
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

fn value(service_id: &str, board_stop_id: &str, offset_seconds: i64) -> NormalizedRealtimeValue {
    let transmitted_at = NOW;
    NormalizedRealtimeValue {
        id: format!("arrival:{service_id}:{board_stop_id}"),
        received_at: transmitted_at,
        normalized_at: transmitted_at,
        provider_published_at: Some(transmitted_at),
        source_change_at: None,
        freshness: Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "test".into(),
            evaluated_at: transmitted_at,
        },
        value: RealtimeValue::ArrivalEstimate(ArrivalEstimate {
            transit_service_id: TransitServiceId::new(service_id),
            boarding_point_id: BoardingPointId::new(board_stop_id),
            predicted_arrival_at: Some(transmitted_at + Duration::seconds(offset_seconds)),
            state: ArrivalState::Predicted,
            transmitted_at,
        }),
    }
}

fn query(service_id: &str, board_stop_id: &str, after: OffsetDateTime) -> DepartureQuery {
    DepartureQuery::new(
        TransitServiceId::new(service_id),
        BoardingPointId::new(board_stop_id),
        after,
        3,
    )
}

fn provider(values: Vec<NormalizedRealtimeValue>) -> BatchProvider {
    BatchProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        queries: Arc::new(Mutex::new(Vec::new())),
        values: Arc::new(values),
        delay: StdDuration::ZERO,
        error: None,
    }
}

#[tokio::test]
async fn twenty_logical_bus_needs_use_one_provider_batch() {
    let service_ids = (0..20)
        .map(|index| TransitServiceId::new(format!("service-{index}")))
        .collect::<Vec<_>>();
    let values = service_ids
        .iter()
        .map(|service_id| value(service_id.as_str(), "stop-x", 60))
        .collect::<Vec<_>>();
    let upstream = provider(values);
    let calls = upstream.calls.clone();
    let queries = upstream.queries.clone();
    let resolver = CompositeDepartureResolver::with_schedule_fallback(
        Arc::new(network_for(&service_ids)),
        Arc::new(RealtimeDepartureResolver::new(
            Arc::new(upstream),
            Arc::new(network_for(&service_ids)),
        )),
    );
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
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let batches = queries.lock().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].transit_service_ids.len(), 20);
    assert_eq!(
        batches[0].boarding_point_ids,
        vec![BoardingPointId::new("stop-x")]
    );
}

#[tokio::test]
async fn realtime_effective_trip_uses_indexed_static_shape_without_identity() {
    let service_id = TransitServiceId::new("service-a");
    let network = Arc::new(network_for(std::slice::from_ref(&service_id)));
    let resolver = RealtimeDepartureResolver::new(
        Arc::new(provider(vec![value("service-a", "stop-x", 60)])),
        network,
    );

    let departures = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await;

    let trip = &departures.unwrap()[0];
    assert_eq!(trip.scheduled_trip_id, None);
    assert_eq!(trip.stop_times[0].departure_at, NOW + Duration::seconds(60));
    assert_eq!(trip.stop_times[1].arrival_at, NOW + Duration::seconds(660));
    assert!(
        trip.stop_times
            .windows(2)
            .all(|times| times[0].departure_at <= times[1].arrival_at)
    );
}

#[tokio::test]
async fn realtime_effective_trip_uses_estimated_segments_without_static_shape() {
    let service_id = TransitServiceId::new("service-a");
    let mut network = network_for(std::slice::from_ref(&service_id));
    network.trips.clear();
    let resolver = RealtimeDepartureResolver::new(
        Arc::new(provider(vec![value("service-a", "stop-x", 60)])),
        Arc::new(network),
    );

    let departures = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await;

    let trip = &departures.unwrap()[0];
    assert_eq!(trip.scheduled_trip_id, None);
    assert_eq!(trip.stop_times[1].arrival_at, NOW + Duration::seconds(180));
    assert!(
        trip.stop_times
            .windows(2)
            .all(|times| times[0].departure_at <= times[1].arrival_at)
    );
}

#[tokio::test]
async fn malformed_static_timing_shape_is_rejected_for_realtime_expansion() {
    let service_id = TransitServiceId::new("service-a");
    let mut network = network_for(std::slice::from_ref(&service_id));
    network.trips[0].stop_times = vec![
        ScheduledStopTime::new(10_000, 10_100),
        ScheduledStopTime::new(10_050, 10_050),
    ];
    let resolver = RealtimeDepartureResolver::new(
        Arc::new(provider(vec![value("service-a", "stop-x", 60)])),
        Arc::new(network),
    );

    let departures = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await;

    assert!(departures.unwrap().is_empty());
}

#[tokio::test]
async fn bus_needs_are_batched_per_realtime_scope() {
    let service_ids = (0..33)
        .map(|index| TransitServiceId::new(format!("service-{index}")))
        .collect::<Vec<_>>();
    let scopes = service_ids
        .iter()
        .enumerate()
        .map(|(index, service_id)| {
            (
                service_id.as_str(),
                if index < 29 { "Taipei" } else { "NewTaipei" },
            )
        })
        .collect::<Vec<_>>();
    let values = service_ids
        .iter()
        .map(|service_id| value(service_id.as_str(), "stop-x", 60))
        .collect::<Vec<_>>();
    let upstream = provider(values);
    let calls = upstream.calls.clone();
    let queries = upstream.queries.clone();
    let network = Arc::new(network_for_with_scopes(&service_ids, &scopes));
    let resolver = RealtimeDepartureResolver::new(Arc::new(upstream), network);
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
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    let batches = queries.lock().unwrap();
    assert_eq!(batches.len(), 2);
    let batch_sizes = batches
        .iter()
        .map(|batch| {
            (
                batch.scope.clone().unwrap(),
                batch.transit_service_ids.len(),
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(batch_sizes.get("Taipei"), Some(&29));
    assert_eq!(batch_sizes.get("NewTaipei"), Some(&4));
}

#[tokio::test]
async fn duplicate_pairs_are_fetched_once_and_results_are_partitioned_exactly() {
    let service_ids = [
        TransitServiceId::new("service-a"),
        TransitServiceId::new("service-b"),
    ];
    let upstream = provider(vec![
        value("service-a", "stop-x", 60),
        value("service-b", "stop-y", 120),
        value("service-a", "stop-y", 180),
        value("service-b", "stop-x", 240),
    ]);
    let calls = upstream.calls.clone();
    let resolver =
        RealtimeDepartureResolver::new(Arc::new(upstream), Arc::new(network_for(&service_ids)));
    let needs = vec![
        query("service-a", "stop-x", NOW),
        query("service-a", "stop-x", NOW + Duration::seconds(90)),
        query("service-b", "stop-y", NOW),
    ];

    let results = resolver.next_departures_batch(needs).await;

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        results[0].as_ref().unwrap()[0].service_id,
        TransitServiceId::new("service-a")
    );
    assert_eq!(
        results[0].as_ref().unwrap()[0].stop_times[0].stop_id,
        BoardingPointId::new("stop-x")
    );
    assert!(results[1].as_ref().unwrap().is_empty());
    assert_eq!(
        results[2].as_ref().unwrap()[0].service_id,
        TransitServiceId::new("service-b")
    );
    assert!(
        results[2].as_ref().unwrap()[0]
            .stop_times
            .iter()
            .any(|stop_time| stop_time.stop_id == BoardingPointId::new("stop-y"))
    );
}

#[tokio::test]
async fn directional_service_ids_do_not_cross_satisfy_each_other() {
    let direction_zero = "route::direction=0";
    let direction_one = "route::direction=1";
    let service_ids = [
        TransitServiceId::new(direction_zero),
        TransitServiceId::new(direction_one),
    ];
    let upstream = provider(vec![
        value(direction_zero, "stop-x", 60),
        value(direction_one, "stop-x", 120),
    ]);
    let resolver =
        RealtimeDepartureResolver::new(Arc::new(upstream), Arc::new(network_for(&service_ids)));

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
async fn identical_batches_coalesce_and_overlapping_pairs_reuse_the_pair_cache() {
    let service_ids = [
        TransitServiceId::new("service-a"),
        TransitServiceId::new("service-b"),
    ];
    let upstream = provider(vec![
        value("service-a", "stop-x", 60),
        value("service-b", "stop-y", 120),
    ]);
    let calls = upstream.calls.clone();
    let cached = Arc::new(CachedRealtimeProvider::with_ttl(
        Arc::new(BatchProvider {
            delay: StdDuration::from_millis(25),
            ..upstream
        }),
        StdDuration::from_secs(30),
    ));
    let first = ArrivalQuery::new(
        service_ids.clone(),
        [
            BoardingPointId::new("stop-x"),
            BoardingPointId::new("stop-y"),
        ],
    );
    let second = ArrivalQuery::for_ride(
        TransitServiceId::new("service-a"),
        BoardingPointId::new("stop-x"),
    );
    let left = cached.clone();
    let right = cached.clone();
    let first_left = first.clone();
    let first_right = first;
    let concurrent = tokio::join!(
        async move { left.arrival_estimates(first_left).await },
        async move { right.arrival_estimates(first_right).await },
    );
    assert!(concurrent.0.is_ok() && concurrent.1.is_ok());
    cached.arrival_estimates(second).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn pair_cache_ttl_expiry_refreshes_the_upstream_batch() {
    let service_id = TransitServiceId::new("service-a");
    let upstream = provider(vec![value("service-a", "stop-x", 60)]);
    let calls = upstream.calls.clone();
    let cached = CachedRealtimeProvider::with_ttl(Arc::new(upstream), StdDuration::from_millis(10));
    let query = ArrivalQuery::for_ride(service_id, BoardingPointId::new("stop-x"));

    cached.arrival_estimates(query.clone()).await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    cached.arrival_estimates(query).await.unwrap();

    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn rate_limit_returns_without_waiting_for_quota() {
    let service_ids = [TransitServiceId::new("service-a")];
    let upstream = BatchProvider {
        error: Some(RealtimeError::RateLimited),
        ..provider(Vec::new())
    };
    let resolver =
        RealtimeDepartureResolver::new(Arc::new(upstream), Arc::new(network_for(&service_ids)));
    let started = std::time::Instant::now();

    let result = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await;

    assert_eq!(
        result,
        Err(via_server::domain::transit_timing::DepartureError::RateLimited)
    );
    assert!(started.elapsed() < StdDuration::from_secs(1));
}

#[tokio::test]
async fn rate_limited_bus_realtime_falls_back_to_static_schedule() {
    let service_ids = [TransitServiceId::new("service-a")];
    let network = Arc::new(network_for(&service_ids));
    let upstream = BatchProvider {
        error: Some(RealtimeError::RateLimited),
        ..provider(Vec::new())
    };
    let resolver = CompositeDepartureResolver::with_schedule_fallback(
        network.clone(),
        Arc::new(RealtimeDepartureResolver::new(
            Arc::new(upstream),
            network.clone(),
        )),
    );

    let result = resolver
        .next_departures(query("service-a", "stop-x", NOW))
        .await
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].timing_basis,
        via_server::domain::transit_timing::TimingBasis::Schedule
    );
}
