use std::{collections::HashMap, sync::Arc};

use time::{Duration, Time, macros::datetime};

use super::*;
use crate::{domain::ids::BoardingPointId, street::FakeStreetRouter};
mod bikeshare;
mod evaluation;
mod pareto_reference;
mod pruning;
mod transit;
mod walking;
use crate::{
    domain::location::GeoCoordinate,
    domain::{
        bikeshare::BikeWalkingAccess,
        ids::{BikeStationId, EffectiveTripId, TransitPatternId},
        transit::{TransferEdge, TransitMode, TransitNetwork, TransitPattern, TransitStop},
        transit_runtime::TransitRuntime,
        transit_timing::{EffectiveStopTime, TimingBasis, TimingReliability},
    },
    routing::{
        AccessPoint, EgressPoint,
        corridors::{StaticCorridor, StaticCorridorStep, StaticCostSource, generate},
        corridors::{TransferShortcutIndex, TransitTopologyIndex},
    },
};

fn stop(id: &str) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        id,
        GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    )
}

fn pattern(id: &str, mode: TransitMode, stops: &[&str]) -> TransitPattern {
    TransitPattern::fixture(
        TransitPatternId::new(id),
        mode,
        id,
        stops.last().copied().unwrap_or_default(),
        stops
            .iter()
            .map(|stop| BoardingPointId::new(*stop))
            .collect(),
    )
}

fn network(
    patterns: Vec<TransitPattern>,
    stop_ids: &[&str],
    transfers: Vec<TransferEdge>,
) -> Arc<TransitRuntime> {
    Arc::new(TransitRuntime::new(Arc::new(
        TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            stop_ids.iter().map(|id| stop(id)).collect(),
            patterns,
            Vec::new(),
        )
        .with_transfer_edges(transfers),
    )))
}

fn query(modes: &[TransitMode], max_transfers: usize, max_corridors: usize) -> PlanningQuery {
    PlanningQuery::new(
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
        vec![AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
        crate::config::ViaConfig::default().planner_limits(),
    )
    .with_allowed_transit_modes(modes.iter().copied())
    .with_limits(crate::routing::PlannerLimits {
        max_transfers,
        max_candidates: 16,
        max_departures_per_query: 3,
        max_journey_duration_seconds: 4 * 60 * 60,
        ..crate::routing::PlannerLimits::default()
    })
    .with_max_static_corridors(max_corridors)
}

fn candidate_corridor(network: &TransitRuntime, query: &PlanningQuery) -> RetainedCorridorSet {
    let topology = TransitTopologyIndex::new(network.network());
    let transfers = TransferShortcutIndex::from_network(network.network());
    generate(network, &topology, &transfers, query).0
}

fn trip(id: &str, pattern_id: &str, stops: &[&str], minute_offsets: &[i64]) -> EffectiveTrip {
    assert_eq!(stops.len(), minute_offsets.len());
    EffectiveTrip {
        id: EffectiveTripId::new(id),
        pattern_id: TransitPatternId::new(pattern_id),
        scheduled_trip_id: None,
        stop_times: stops
            .iter()
            .zip(minute_offsets)
            .map(|(stop, minute)| EffectiveStopTime {
                stop_id: BoardingPointId::new(*stop),
                arrival_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(*minute),
                departure_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(*minute),
            })
            .collect(),
        timing_basis: TimingBasis::RealtimePrediction,
        timing_reliability: TimingReliability::Reliable,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    }
}

fn realized_ride(from_anchor: usize, trip: EffectiveTrip) -> JourneyMovement {
    JourneyMovement {
        from_anchor,
        to_anchor: from_anchor + 1,
        departure_at: trip.stop_times[0].departure_at,
        arrival_at: trip.stop_times[1].arrival_at,
        kind: JourneyMovementKind::Transit {
            trip: Arc::new(trip),
            board_index: 0,
            alight_index: 1,
        },
    }
}

fn static_leg(pattern_id: &str, board: &str, alight: &str) -> StaticCorridorStep {
    StaticCorridorStep::Transit(StaticTransitLeg {
        pattern_id: TransitPatternId::new(pattern_id),
        board_stop_id: BoardingPointId::new(board),
        board_index: 0,
        alight_stop_id: BoardingPointId::new(alight),
        alight_index: 1,
        cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
    })
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CorridorMovement {
    from_anchor: usize,
    to_anchor: usize,
    from: BoardingPointId,
    to: BoardingPointId,
}

type TestBikeRoutes = HashMap<CorridorMovement, Vec<BikeShareMovement>>;

fn corridor_movement(
    from_anchor: usize,
    to_anchor: usize,
    from: &str,
    to: &str,
) -> CorridorMovement {
    CorridorMovement {
        from_anchor,
        to_anchor,
        from: BoardingPointId::new(from),
        to: BoardingPointId::new(to),
    }
}

#[derive(Default)]
struct TestBikeSource {
    routes: TestBikeRoutes,
    calls: std::sync::atomic::AtomicUsize,
    realizations: std::sync::atomic::AtomicUsize,
    realized: std::sync::Mutex<std::collections::HashSet<(BoardingPointId, BoardingPointId)>>,
}

impl TestBikeSource {
    fn new(routes: TestBikeRoutes) -> Self {
        Self {
            routes,
            ..Self::default()
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn record_stats(&self, stats: &mut CorridorEvaluationStats) {
        let realizations = self.realizations.load(std::sync::atomic::Ordering::SeqCst);
        stats.bike_requests = self.realized.lock().unwrap().len();
        stats.bike_cache_hits = realizations - stats.bike_requests;
    }
}

#[async_trait::async_trait]
impl BikeMovementSource for TestBikeSource {
    async fn movements(
        &self,
        from: &CorridorAnchor,
        to: &CorridorAnchor,
        from_anchor: usize,
        to_anchor: usize,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (Some(from), Some(to), Some(corridor_from), Some(corridor_to)) = (
            from.boarding_point(),
            to.boarding_point(),
            from_anchor.checked_sub(1),
            to_anchor.checked_sub(1),
        ) else {
            return Ok(Vec::new());
        };
        let movement = CorridorMovement {
            from_anchor: corridor_from,
            to_anchor: corridor_to,
            from: from.clone(),
            to: to.clone(),
        };
        let Some(options) = self.routes.get(&movement).filter(|items| !items.is_empty()) else {
            return Ok(Vec::new());
        };
        self.realizations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.realized
            .lock()
            .unwrap()
            .insert((from.clone(), to.clone()));
        Ok(options
            .iter()
            .cloned()
            .map(|mut movement| {
                movement.from_anchor = from_anchor;
                movement.to_anchor = to_anchor;
                movement
            })
            .collect())
    }
}

fn test_evaluator<'a>(
    network: &'a TransitRuntime,
    retained: &'a RetainedCorridorSet,
    snapshot: &'a TimingSnapshot,
    query: &'a PlanningQuery,
    street: &'a MatrixCache,
    bike: Option<&'a dyn BikeMovementSource>,
) -> CorridorEvaluator<'a> {
    CorridorEvaluator {
        network,
        retained,
        timing: snapshot,
        query,
        street,
        bike,
    }
}

fn fake_street() -> MatrixCache {
    MatrixCache::new(Arc::new(FakeStreetRouter::builder().build()))
}

async fn evaluate_corridors(
    network: &TransitRuntime,
    retained: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: &PlanningQuery,
    street: &MatrixCache,
    bike: Option<&dyn BikeMovementSource>,
    corridors: &[EvaluationCorridor],
) -> Result<Evaluation, RoutingError> {
    CorridorEvaluator {
        network,
        retained,
        timing: snapshot,
        query,
        street,
        bike,
    }
    .evaluate(corridors)
    .await
}

fn transit_corridors(
    network: &TransitRuntime,
    retained: &RetainedCorridorSet,
    query: &PlanningQuery,
) -> Result<Vec<EvaluationCorridor>, RoutingError> {
    retained
        .retained()
        .iter()
        .map(|corridor| EvaluationCorridor::from_transit(network, corridor, query))
        .collect()
}

fn run_test_evaluation(
    network: &TransitRuntime,
    retained: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
    bike: Option<&dyn BikeMovementSource>,
) -> Result<(Vec<RealizedJourney>, CorridorEvaluationStats), RoutingError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime")
        .block_on(async {
            if query.origin_access.is_empty() || query.destination_egress.is_empty() {
                return Err(RoutingError::InvalidQuery);
            }
            let corridors = transit_corridors(network, retained, &query)?;
            let street = fake_street();
            let evaluation = evaluate_corridors(
                network, retained, snapshot, &query, &street, bike, &corridors,
            )
            .await?;
            Ok((evaluation.journeys, evaluation.stats))
        })
}

fn evaluate(
    network: &TransitRuntime,
    retained: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
) -> Result<Vec<RealizedJourney>, RoutingError> {
    evaluate_with_stats(network, retained, snapshot, query).map(|(journeys, _)| journeys)
}

fn evaluate_with_stats(
    network: &TransitRuntime,
    retained: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
) -> Result<(Vec<RealizedJourney>, CorridorEvaluationStats), RoutingError> {
    run_test_evaluation(network, retained, snapshot, query, None)
}

fn evaluate_with_test_bike_routes(
    network: &TransitRuntime,
    retained: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
    routes: &TestBikeRoutes,
) -> Result<(Vec<RealizedJourney>, CorridorEvaluationStats), RoutingError> {
    let bike = TestBikeSource::new(routes.clone());
    let (journeys, mut stats) =
        run_test_evaluation(network, retained, snapshot, query, Some(&bike))?;
    bike.record_stats(&mut stats);
    Ok((journeys, stats))
}

fn prune_states_reference(mut states: Vec<EvaluationState>, limit: usize) -> Vec<EvaluationState> {
    states.sort_by_cached_key(|state| {
        let metrics = state.metrics();
        (
            state.ready_at,
            metrics.walking_seconds,
            metrics.transfer_count,
        )
    });
    let mut frontier = Vec::<EvaluationState>::new();
    for state in states {
        let metrics = state.metrics();
        if frontier.iter().any(|known| {
            pareto::same_capability(known, &state) && pareto::no_worse(&known.metrics(), &metrics)
        }) {
            continue;
        }
        frontier.retain(|known| {
            !(pareto::same_capability(&state, known)
                && pareto::no_worse(&metrics, &known.metrics()))
        });
        frontier.push(state);
    }
    frontier.truncate(limit.max(1));
    frontier
}

fn bike_movement(from_anchor: usize, to_anchor: usize, seconds: u32) -> BikeShareMovement {
    BikeShareMovement::new(
        from_anchor,
        to_anchor,
        BikeStationId::new(format!("pickup-{from_anchor}")),
        BikeStationId::new(format!("dropoff-{to_anchor}")),
        BikeWalkingAccess::new(0, 0),
        crate::routing::BikeRide {
            duration_seconds: seconds,
            distance_metres: seconds,
        },
        BikeWalkingAccess::new(0, 0),
    )
    .unwrap()
}

struct CountingWalkingRouter {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    error: Option<crate::street::StreetRoutingError>,
}

#[async_trait::async_trait]
impl crate::street::StreetRouter for CountingWalkingRouter {
    async fn matrix(
        &self,
        query: crate::street::MatrixQuery,
    ) -> Result<crate::street::MatrixResult, crate::street::StreetRoutingError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(query.travel_mode, crate::street::StreetTravelMode::Walking);
        assert_eq!(query.sources.len(), 1);
        assert_eq!(query.targets.len(), 1);
        assert!(query.constraints.time.is_none());
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(crate::street::MatrixResult {
            cells: vec![vec![crate::street::MatrixCell::Reachable {
                duration: crate::street::Seconds(300),
                distance: crate::street::Metres(2000),
            }]],
        })
    }
    async fn route(
        &self,
        _: crate::street::RouteQuery,
    ) -> Result<crate::street::StreetPath, crate::street::StreetRoutingError> {
        unreachable!("Phase 2 walking uses Matrix")
    }
}

async fn evaluate_walking_replacement(
    transit_minutes: i64,
    error: Option<crate::street::StreetRoutingError>,
    latest_seconds: Option<i64>,
    duplicate: bool,
    with_bike: bool,
) -> (Result<Evaluation, RoutingError>, usize) {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut static_network = network.network().clone();
    static_network.stops[1].coordinate = GeoCoordinate {
        latitude: 25.02,
        longitude: 121.5,
    };
    let network = Arc::new(TransitRuntime::new(Arc::new(static_network)));
    let mut query = query(&[TransitMode::Bus], 0, 10);
    query.latest_arrival =
        latest_seconds.map(|seconds| query.departure_time + Duration::seconds(seconds));
    let retained = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip(
            "ride",
            "bus",
            &["origin", "destination"],
            &[0, transit_minutes],
        )],
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let street = MatrixCache::new(Arc::new(CountingWalkingRouter {
        calls: calls.clone(),
        error,
    }));
    let bike = with_bike.then(|| {
        TestBikeSource::new(HashMap::from([(
            corridor_movement(0, 1, "origin", "destination"),
            vec![bike_movement(0, 1, 240)],
        )]))
    });
    let make_corridor =
        || EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let corridors = if duplicate {
        vec![make_corridor(), make_corridor()]
    } else {
        vec![make_corridor()]
    };
    let result = evaluate_corridors(
        &network,
        &retained,
        &snapshot,
        &query,
        &street,
        bike.as_ref().map(|bike| bike as &dyn BikeMovementSource),
        &corridors,
    )
    .await;
    (result, calls.load(std::sync::atomic::Ordering::SeqCst))
}

struct DirectBikePlanner {
    movement: BikeShareMovement,
}

#[async_trait::async_trait]
impl BikeMovementSource for DirectBikePlanner {
    async fn movements(
        &self,
        from: &CorridorAnchor,
        to: &CorridorAnchor,
        _: usize,
        _: usize,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        Ok((matches!(from.kind, JourneyAnchorKind::Origin)
            && matches!(to.kind, JourneyAnchorKind::Destination))
        .then(|| self.movement.clone())
        .into_iter()
        .collect())
    }
}

struct SlowSecondWalkingRouter {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl crate::street::StreetRouter for SlowSecondWalkingRouter {
    async fn matrix(
        &self,
        _: crate::street::MatrixQuery,
    ) -> Result<crate::street::MatrixResult, crate::street::StreetRoutingError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Ok(crate::street::MatrixResult {
            cells: vec![vec![crate::street::MatrixCell::Reachable {
                duration: crate::street::Seconds(300),
                distance: crate::street::Metres(1000),
            }]],
        })
    }
    async fn route(
        &self,
        _: crate::street::RouteQuery,
    ) -> Result<crate::street::StreetPath, crate::street::StreetRoutingError> {
        unreachable!("Phase 2 walking uses Matrix")
    }
}

struct OutcomeBikeProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    error: Option<crate::street::StreetRoutingError>,
    expire_deadline: bool,
}

#[async_trait::async_trait]
impl BikeMovementSource for OutcomeBikeProvider {
    async fn movements(
        &self,
        _: &CorridorAnchor,
        _: &CorridorAnchor,
        from_anchor: usize,
        to_anchor: usize,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.expire_deadline {
            tokio::time::advance(std::time::Duration::from_secs(2)).await;
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(vec![bike_movement(from_anchor, to_anchor, 60)])
    }
}

struct DeadlineWalkingRouter;

#[async_trait::async_trait]
impl crate::street::StreetRouter for DeadlineWalkingRouter {
    async fn matrix(
        &self,
        _: crate::street::MatrixQuery,
    ) -> Result<crate::street::MatrixResult, crate::street::StreetRoutingError> {
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        Ok(crate::street::MatrixResult {
            cells: vec![vec![crate::street::MatrixCell::Reachable {
                duration: crate::street::Seconds(60),
                distance: crate::street::Metres(100),
            }]],
        })
    }
    async fn route(
        &self,
        _: crate::street::RouteQuery,
    ) -> Result<crate::street::StreetPath, crate::street::StreetRoutingError> {
        unreachable!()
    }
}

#[path = "parallel_tests.rs"]
mod parallel_tests;

fn pruning_state(movements: Vec<JourneyMovement>) -> EvaluationState {
    let ready_at = movements
        .last()
        .map_or(datetime!(2026-09-21 10:00 UTC), |movement| {
            movement.arrival_at
        });
    let last_was_transit = movements
        .last()
        .is_some_and(|movement| matches!(movement.kind, JourneyMovementKind::Transit { .. }));
    EvaluationState {
        ready_at,
        last_was_transit,
        active_bike_rental: None,
        bike_reuse_available_at: None,
        movements,
    }
}

fn pruning_transit_state(
    id: &str,
    departure_minute: i64,
    arrival_minute: i64,
    timing_basis: TimingBasis,
    timing_reliability: TimingReliability,
    evidence_id: Option<&str>,
) -> EvaluationState {
    let mut effective_trip = trip(
        id,
        "bus",
        &["origin", "destination"],
        &[departure_minute, arrival_minute],
    );
    effective_trip.timing_basis = timing_basis;
    effective_trip.timing_reliability = timing_reliability;
    if let Some(evidence_id) = evidence_id {
        let transmitted_at = datetime!(2026-09-21 10:00 UTC) + Duration::minutes(departure_minute);
        effective_trip
            .evidence
            .push(crate::domain::realtime::NormalizedRealtimeValue {
                freshness: crate::domain::realtime::FreshnessState::Fresh,
                value: crate::domain::realtime::ArrivalEstimate {
                    transit_pattern_id: TransitPatternId::new("bus"),
                    boarding_point_id: BoardingPointId::new(evidence_id),
                    predicted_arrival_at: Some(
                        datetime!(2026-09-21 10:00 UTC) + Duration::minutes(arrival_minute),
                    ),
                    state: crate::domain::realtime::ArrivalState::Predicted,
                    transmitted_at,
                },
            });
    }
    pruning_state(vec![realized_ride(0, effective_trip)])
}

fn pruning_walk_state(distance_metres: u32) -> EvaluationState {
    let transit = realized_ride(
        0,
        trip("walk-prefix", "bus", &["origin", "destination"], &[1, 3]),
    );
    let departure_at = transit.arrival_at;
    let walking = JourneyMovement {
        from_anchor: 1,
        to_anchor: 2,
        departure_at,
        arrival_at: departure_at + Duration::minutes(2),
        kind: JourneyMovementKind::Walking {
            kind: WalkingMovementKind::Transfer,
            duration_seconds: 120,
            distance_metres,
        },
    };
    pruning_state(vec![transit, walking])
}

fn pruning_bike_state(rental_started_at_delta: i64, reuse_delta: i64) -> EvaluationState {
    let transit = realized_ride(
        0,
        trip("bike-prefix", "bus", &["origin", "destination"], &[1, 3]),
    );
    let started_at = transit.arrival_at + Duration::seconds(rental_started_at_delta);
    let bike = bike_movement(1, 2, 300);
    let bike_step = JourneyMovement {
        from_anchor: 1,
        to_anchor: 2,
        departure_at: started_at,
        arrival_at: started_at + Duration::seconds(i64::from(bike.total_seconds)),
        kind: JourneyMovementKind::BikeShare {
            movement: bike.clone(),
        },
    };
    let mut state = pruning_state(vec![transit.clone(), bike_step]);
    state.active_bike_rental = Some(super::state::ActiveBikeRental {
        start_anchor: 1,
        started_at,
        prefix_movements: vec![transit],
        movement: bike.clone(),
    });
    state.bike_reuse_available_at =
        Some(started_at + Duration::minutes(20) + Duration::seconds(reuse_delta));
    state
}

fn pruning_state_corpus() -> Vec<EvaluationState> {
    let mut states = vec![
        pruning_transit_state(
            "tie-first",
            1,
            6,
            TimingBasis::RealtimePrediction,
            TimingReliability::Reliable,
            None,
        ),
        pruning_transit_state(
            "tie-second",
            1,
            6,
            TimingBasis::RealtimePrediction,
            TimingReliability::Reliable,
            None,
        ),
        pruning_transit_state(
            "different-basis",
            1,
            6,
            TimingBasis::Schedule,
            TimingReliability::Reliable,
            None,
        ),
        pruning_transit_state(
            "different-evidence",
            1,
            6,
            TimingBasis::RealtimePrediction,
            TimingReliability::Reliable,
            Some("origin-a"),
        ),
        pruning_transit_state(
            "different-reliability",
            1,
            6,
            TimingBasis::RealtimePrediction,
            TimingReliability::Fallback,
            None,
        ),
        pruning_walk_state(120),
        pruning_walk_state(120),
        pruning_walk_state(121),
        pruning_bike_state(0, 0),
        pruning_bike_state(0, 1),
        pruning_bike_state(1, 0),
    ];
    let mut transfer_state = pruning_state(vec![
        realized_ride(
            0,
            trip("transfer-first", "bus", &["origin", "destination"], &[1, 3]),
        ),
        realized_ride(
            1,
            trip(
                "transfer-second",
                "bus",
                &["origin", "destination"],
                &[4, 6],
            ),
        ),
    ]);
    transfer_state.last_was_transit = true;
    states.push(transfer_state);
    states
}
