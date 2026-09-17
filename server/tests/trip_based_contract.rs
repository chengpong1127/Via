use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::{Duration, Time, Weekday, macros::datetime};
use via_server::domain::ids::{BoardingPointId, EffectiveTripId, TransitServiceId};
use via_server::domain::transit::{
    ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService, TransitStop,
};
use via_server::domain::transit_timing::{
    DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
    TimingBasis,
};
use via_server::providers::timing::CompositeDepartureResolver;
use via_server::routing::algorithm::RoutingAlgorithm;
use via_server::routing::transfers::{TransferIndex, TransferShortcut, TransferShortcutIndex};
use via_server::routing::trip_based::TripBasedRouter;
use via_server::routing::{AccessPoint, AlgorithmQuery, EgressPoint, JourneySegment};

fn stop(id: &str) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        id,
        via_server::api::RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    )
}

fn network(service_ids_and_patterns: &[(&str, &[&str])]) -> Arc<TransitNetwork> {
    let stop_ids = service_ids_and_patterns
        .iter()
        .flat_map(|(_, pattern)| pattern.iter().copied())
        .collect::<std::collections::BTreeSet<_>>();
    let stops = stop_ids.into_iter().map(stop).collect::<Vec<_>>();
    let services = service_ids_and_patterns
        .iter()
        .map(|(service_id, pattern)| {
            TransitService::new(
                TransitServiceId::new(*service_id),
                TransitMode::Bus,
                *service_id,
                pattern.last().copied().unwrap_or_default(),
                pattern.iter().map(|id| BoardingPointId::new(*id)).collect(),
            )
        })
        .collect();
    Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        stops,
        services,
        Vec::new(),
        Vec::new(),
    ))
}

fn trip(
    id: &str,
    service_id: &str,
    stops: &[&str],
    departures: &[i64],
    arrivals: &[i64],
    realtime: bool,
) -> EffectiveTrip {
    let stop_times = stops
        .iter()
        .enumerate()
        .map(|(index, stop_id)| EffectiveStopTime {
            stop_id: BoardingPointId::new(*stop_id),
            arrival_at: datetime!(2026-08-31 10:00 UTC) + Duration::seconds(arrivals[index]),
            departure_at: datetime!(2026-08-31 10:00 UTC) + Duration::seconds(departures[index]),
        })
        .collect();
    EffectiveTrip {
        id: EffectiveTripId::new(id),
        service_id: TransitServiceId::new(service_id),
        scheduled_trip_id: (!realtime).then(|| via_server::domain::ids::ScheduledTripId::new(id)),
        stop_times,
        timing_basis: if realtime {
            TimingBasis::RealtimePrediction
        } else {
            TimingBasis::Schedule
        },
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    }
}

#[derive(Clone, Default)]
struct FixtureResolver {
    trips: Arc<HashMap<TransitServiceId, Vec<EffectiveTrip>>>,
    calls: Arc<Mutex<Vec<DepartureQuery>>>,
}

#[async_trait]
impl DepartureResolver for FixtureResolver {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.calls.lock().unwrap().push(query.clone());
        let Some(trips) = self.trips.get(&query.service_id) else {
            return Ok(Vec::new());
        };
        let mut result = trips
            .iter()
            .filter(|trip| {
                trip.stop_index(&query.board_stop_id)
                    .is_some_and(|index| trip.stop_times[index].departure_at >= query.after)
            })
            .cloned()
            .collect::<Vec<_>>();
        result.truncate(query.limit);
        Ok(result)
    }
}

fn resolver(trips: Vec<EffectiveTrip>) -> FixtureResolver {
    let service_id = trips[0].service_id.clone();
    FixtureResolver {
        trips: Arc::new(HashMap::from([(service_id, trips)])),
        ..Default::default()
    }
}

fn query(from: &str, to: &str) -> AlgorithmQuery {
    AlgorithmQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new(from),
            datetime!(2026-08-31 10:00 UTC),
            datetime!(2026-08-31 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new(to), 0, 0)],
        datetime!(2026-08-31 10:00 UTC),
    )
}

#[tokio::test]
async fn scheduled_router_selects_the_earliest_boardable_trip() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![
        trip(
            "late",
            "line",
            &["a", "b"],
            &[900, 1_500],
            &[900, 1_500],
            false,
        ),
        trip(
            "early",
            "line",
            &["a", "b"],
            &[300, 900],
            &[300, 900],
            false,
        ),
    ]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router.route(query("a", "b")).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].arrival_time, datetime!(2026-08-31 10:15 UTC));
    assert!(matches!(
        &candidates[0].segments[0],
        JourneySegment::Transit { .. }
    ));
}

#[tokio::test]
async fn runtime_resolution_is_lazy_and_scans_the_effective_trip_forward() {
    let network = network(&[("explored", &["a", "b"]), ("unused", &["x", "y"])]);
    let resolver = resolver(vec![trip(
        "runtime-bus",
        "explored",
        &["a", "b"],
        &[300, 900],
        &[300, 900],
        true,
    )]);
    let calls = resolver.calls.clone();
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router.route(query("a", "b")).await.unwrap();
    assert_eq!(candidates[0].arrival_time, datetime!(2026-08-31 10:15 UTC));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        calls.lock().unwrap()[0].service_id,
        TransitServiceId::new("explored")
    );
}

#[tokio::test]
async fn a_later_candidate_can_win_when_the_earliest_candidate_is_not_feasible() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![
        trip(
            "too-late",
            "line",
            &["a", "b"],
            &[300, 3_000],
            &[300, 3_000],
            true,
        ),
        trip(
            "usable",
            "line",
            &["a", "b"],
            &[600, 1_800],
            &[600, 1_800],
            true,
        ),
    ]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router
        .route(query("a", "b").with_latest_arrival(datetime!(2026-08-31 10:30 UTC)))
        .await
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].arrival_time, datetime!(2026-08-31 10:30 UTC));
}

#[tokio::test]
async fn walking_transfer_shortcut_expands_a_second_service() {
    let network = network(&[("first", &["a", "b"]), ("second", &["c", "d"])]);
    let resolver = FixtureResolver {
        trips: Arc::new(HashMap::from([
            (
                TransitServiceId::new("first"),
                vec![trip(
                    "first-trip",
                    "first",
                    &["a", "b"],
                    &[60, 600],
                    &[60, 600],
                    false,
                )],
            ),
            (
                TransitServiceId::new("second"),
                vec![trip(
                    "second-trip",
                    "second",
                    &["c", "d"],
                    &[1_200, 1_800],
                    &[1_200, 1_800],
                    false,
                )],
            ),
        ])),
        ..Default::default()
    };
    let transfer = TransferShortcutIndex::new([TransferShortcut::new(
        BoardingPointId::new("b"),
        BoardingPointId::new("c"),
        60,
    )]);
    let router = TripBasedRouter::new(network, Arc::new(resolver), Arc::new(transfer));
    let candidates = router.route(query("a", "d")).await.unwrap();
    assert!(candidates.iter().any(|candidate| {
        candidate.transfer_count == 1
            && candidate
                .segments
                .iter()
                .filter(|segment| matches!(segment, JourneySegment::Transit { .. }))
                .count()
                == 2
    }));
}

#[tokio::test]
async fn trip_reached_from_an_earlier_index_is_not_rescanned() {
    let network = network(&[("line", &["a", "b", "c"])]);
    let resolver = resolver(vec![trip(
        "trip",
        "line",
        &["a", "b", "c"],
        &[60, 300, 600],
        &[60, 300, 600],
        false,
    )]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let query = AlgorithmQuery::new(
        vec![
            AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-08-31 10:00 UTC),
                datetime!(2026-08-31 10:00 UTC),
                0,
                0,
            ),
            AccessPoint::new(
                BoardingPointId::new("b"),
                datetime!(2026-08-31 10:00 UTC),
                datetime!(2026-08-31 10:00 UTC),
                30,
                0,
            ),
        ],
        vec![EgressPoint::new(BoardingPointId::new("c"), 0, 0)],
        datetime!(2026-08-31 10:00 UTC),
    );
    let candidates = router.route(query).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(matches!(
        candidates[0].segments[0],
        JourneySegment::Transit { board_index: 0, .. }
    ));
}

#[tokio::test]
async fn dominated_runtime_departure_is_removed() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![
        trip("early", "line", &["a", "b"], &[60, 600], &[60, 600], true),
        trip("late", "line", &["a", "b"], &[120, 900], &[120, 900], true),
    ]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router.route(query("a", "b")).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].segments[0].service_id(),
        Some(&TransitServiceId::new("line"))
    );
}

#[tokio::test]
async fn pareto_keeps_earlier_extra_transfer_and_later_direct_result() {
    let network = network(&[
        ("first", &["a", "b"]),
        ("second", &["c", "d"]),
        ("direct", &["a", "d"]),
    ]);
    let resolver = FixtureResolver {
        trips: Arc::new(HashMap::from([
            (
                TransitServiceId::new("first"),
                vec![trip(
                    "first-trip",
                    "first",
                    &["a", "b"],
                    &[60, 300],
                    &[60, 300],
                    false,
                )],
            ),
            (
                TransitServiceId::new("second"),
                vec![trip(
                    "second-trip",
                    "second",
                    &["c", "d"],
                    &[600, 900],
                    &[600, 900],
                    false,
                )],
            ),
            (
                TransitServiceId::new("direct"),
                vec![trip(
                    "direct-trip",
                    "direct",
                    &["a", "d"],
                    &[120, 1_500],
                    &[120, 1_500],
                    false,
                )],
            ),
        ])),
        ..Default::default()
    };
    let transfer = TransferShortcutIndex::new([TransferShortcut::new(
        BoardingPointId::new("b"),
        BoardingPointId::new("c"),
        60,
    )]);
    let router = TripBasedRouter::new(network, Arc::new(resolver), Arc::new(transfer));
    let candidates = router.route(query("a", "d")).await.unwrap();
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.transfer_count == 1)
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.transfer_count == 0)
    );
}

#[tokio::test]
async fn duplicate_timing_needs_are_coalesced() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![trip(
        "trip",
        "line",
        &["a", "b"],
        &[60, 600],
        &[60, 600],
        true,
    )]);
    let calls = resolver.calls.clone();
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let base = AccessPoint::new(
        BoardingPointId::new("a"),
        datetime!(2026-08-31 10:00 UTC),
        datetime!(2026-08-31 10:00 UTC),
        0,
        0,
    );
    let query = AlgorithmQuery::new(
        vec![base.clone(), base],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-08-31 10:00 UTC),
    );
    router.route(query).await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[test]
fn transfer_index_preserves_multiple_nondominated_alternatives() {
    let index = TransferShortcutIndex::new([
        TransferShortcut::new(BoardingPointId::new("b"), BoardingPointId::new("c"), 120),
        TransferShortcut::new(BoardingPointId::new("b"), BoardingPointId::new("c"), 180),
    ]);
    assert_eq!(index.transfers_from(&BoardingPointId::new("b")).len(), 2);
}

#[tokio::test]
async fn departure_resolver_falls_back_to_schedule() {
    #[derive(Clone)]
    struct FailingResolver;

    #[async_trait]
    impl DepartureResolver for FailingResolver {
        async fn next_departures(
            &self,
            _query: DepartureQuery,
        ) -> Result<Vec<EffectiveTrip>, DepartureError> {
            Err(DepartureError::TimedOut)
        }
    }

    let stops = vec![stop("a"), stop("b")];
    let services = vec![TransitService::new(
        TransitServiceId::new("line"),
        TransitMode::Bus,
        "line",
        "b",
        vec![BoardingPointId::new("a"), BoardingPointId::new("b")],
    )];
    let schedule = Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        stops,
        services,
        vec![ScheduledTrip::weekdays(
            via_server::domain::ids::ScheduledTripId::new("run"),
            TransitServiceId::new("line"),
            vec![Weekday::Monday],
            vec![
                ScheduledStopTime::new(36_060, 36_060),
                ScheduledStopTime::new(36_120, 36_120),
            ],
        )],
        Vec::new(),
    ));
    let resolver =
        CompositeDepartureResolver::with_schedule_fallback(schedule, Arc::new(FailingResolver));
    let query = DepartureQuery::new(
        TransitServiceId::new("line"),
        BoardingPointId::new("a"),
        datetime!(2026-08-31 02:00 UTC),
        1,
    );
    let departures = resolver.next_departures(query).await.unwrap();
    assert_eq!(departures.len(), 1);
    assert!(matches!(departures[0].timing_basis, TimingBasis::Schedule));
}

#[tokio::test]
async fn algorithm_behavior_is_independent_of_transit_mode() {
    let resolver = resolver(vec![trip(
        "trip",
        "line",
        &["a", "b"],
        &[60, 600],
        &[60, 600],
        false,
    )]);
    let bus_router = TripBasedRouter::new(
        network(&[("line", &["a", "b"])]),
        Arc::new(resolver.clone()),
        Arc::new(TransferShortcutIndex::default()),
    );
    let metro_router = TripBasedRouter::new(
        network_with_mode(&[("line", &["a", "b"])], TransitMode::Metro),
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let (bus, metro) = tokio::join!(
        bus_router.route(query("a", "b")),
        metro_router.route(query("a", "b"))
    );
    assert_eq!(bus.unwrap()[0].signature(), metro.unwrap()[0].signature());
}

fn network_with_mode(
    service_ids_and_patterns: &[(&str, &[&str])],
    mode: TransitMode,
) -> Arc<TransitNetwork> {
    let stop_ids = service_ids_and_patterns
        .iter()
        .flat_map(|(_, pattern)| pattern.iter().copied())
        .collect::<std::collections::BTreeSet<_>>();
    let stops = stop_ids.into_iter().map(stop).collect::<Vec<_>>();
    let services = service_ids_and_patterns
        .iter()
        .map(|(service_id, pattern)| {
            TransitService::new(
                TransitServiceId::new(*service_id),
                mode,
                *service_id,
                pattern.last().copied().unwrap_or_default(),
                pattern.iter().map(|id| BoardingPointId::new(*id)).collect(),
            )
        })
        .collect();
    Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        stops,
        services,
        Vec::new(),
        Vec::new(),
    ))
}
