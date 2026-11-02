use super::*;
use crate::{routing::evaluation::bike::BikeShareSession, street::*};
use std::sync::atomic::{AtomicUsize, Ordering};

struct DelayedMatrix {
    delay: u64,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    error: Option<StreetRoutingError>,
    semaphore: tokio::sync::Semaphore,
    waiting_us: std::sync::atomic::AtomicU64,
    bicycle_calls: AtomicUsize,
    gate: Option<Arc<tokio::sync::Semaphore>>,
    started: Option<Arc<tokio::sync::Semaphore>>,
}

impl DelayedMatrix {
    fn new(delay: u64, error: Option<StreetRoutingError>) -> Self {
        Self {
            delay,
            error,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            semaphore: tokio::sync::Semaphore::new(8),
            waiting_us: std::sync::atomic::AtomicU64::new(0),
            bicycle_calls: AtomicUsize::new(0),
            gate: None,
            started: None,
        }
    }
}

struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[async_trait::async_trait]
impl StreetRouter for DelayedMatrix {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        assert!(query.constraints.time.is_none());
        let waiting = std::time::Instant::now();
        let _permit = self.semaphore.acquire().await.unwrap();
        self.waiting_us
            .fetch_add(waiting.elapsed().as_micros() as u64, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
        if query.travel_mode == StreetTravelMode::Bicycle {
            self.bicycle_calls.fetch_add(1, Ordering::Relaxed);
        }
        let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
        let _active = Active(&self.active);
        if let Some(started) = &self.started {
            started.add_permits(1);
        }
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        if self.delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay)).await;
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(MatrixResult {
            cells: vec![
                vec![
                    MatrixCell::Reachable {
                        duration: Seconds(if query.travel_mode == StreetTravelMode::Walking {
                            300
                        } else {
                            120
                        }),
                        distance: Metres(1000),
                    };
                    query.targets.len()
                ];
                query.sources.len()
            ],
        })
    }
    async fn route(&self, _: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        panic!("unexpected geometry")
    }
}

struct Measured {
    calls: usize,
    peak: usize,
    journeys: Vec<String>,
    stats: CorridorEvaluationStats,
    bicycle_calls: usize,
}

async fn measured_case(
    delay: u64,
    error: Option<StreetRoutingError>,
    concurrency: usize,
    variant: &str,
) -> Measured {
    measured_corridors_case(delay, error, concurrency, 1, 1, variant).await
}

async fn measured_corridors_case(
    delay: u64,
    error: Option<StreetRoutingError>,
    concurrency: usize,
    corridor_concurrency: usize,
    corridor_count: usize,
    variant: &str,
) -> Measured {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 1);
    query.limits.max_concurrent_phase2_states = concurrency;
    query.limits.max_concurrent_phase2_corridors = corridor_concurrency;
    let retained = candidate_corridor(&network, &query);
    let original =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let corridors = (0..corridor_count)
        .map(|_| {
            EvaluationCorridor::new(
                original.anchors().to_vec(),
                vec![
                    PlannedMovement::RetainedWalking {
                        kind: WalkingMovementKind::Access,
                        duration_seconds: 0,
                        distance_metres: 0,
                        access_available_at: Some(query.departure_time),
                    },
                    PlannedMovement::Transit(
                        retained.retained()[0]
                            .transit_legs()
                            .next()
                            .unwrap()
                            .clone(),
                    ),
                    PlannedMovement::WalkingToResolve,
                ],
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        (0..3)
            .map(|i| {
                trip(
                    &format!("trip-{i}"),
                    "bus",
                    &["origin", "destination"],
                    &[1 + i, 2 + i],
                )
            })
            .collect(),
    );
    let provider = Arc::new(DelayedMatrix::new(delay, error));
    let matrix = Arc::new(MatrixCache::new(provider.clone()));
    let bike =
        (variant != "walking").then(|| real_bike_session(matrix.clone(), query.departure_time));
    if variant == "bike" {
        snapshot = TimingSnapshot::default();
    }
    let result = evaluate_corridors(
        &network,
        &retained,
        &snapshot,
        &query,
        &matrix,
        bike.as_ref().map(|bike| bike as &dyn BikeMovementSource),
        &corridors,
    )
    .await
    .unwrap();
    Measured {
        calls: provider.calls.load(Ordering::Relaxed),
        peak: provider.peak.load(Ordering::Relaxed),
        journeys: result.journeys.iter().map(|j| format!("{j:?}")).collect(),
        stats: result.stats,
        bicycle_calls: provider.bicycle_calls.load(Ordering::Relaxed),
    }
}

#[tokio::test]
async fn successful_evaluations_preserve_ordered_journeys_and_work_at_all_limits() {
    let reference = measured_case(1, None, 1, "walking").await;
    for concurrency in [2, 4, 8] {
        let actual = measured_case(1, None, concurrency, "walking").await;
        assert_eq!(actual.journeys, reference.journeys);
        assert_eq!(
            actual.stats.states_expanded,
            reference.stats.states_expanded
        );
        assert_eq!(
            actual.stats.successors_generated,
            reference.stats.successors_generated
        );
        assert_eq!(
            actual.stats.corridors_evaluated,
            reference.stats.corridors_evaluated
        );
        assert_eq!(
            actual.stats.final_candidates,
            reference.stats.final_candidates
        );
        assert_eq!(actual.calls, reference.calls);
        assert!(actual.peak >= 1);
        assert!(actual.peak <= concurrency);
        assert_eq!(actual.stats.matrix_requests, actual.calls);
    }
}

#[tokio::test]
async fn edge_collector_restores_input_order_before_pareto_pruning() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut expected = None;
    for concurrency in [1, 2, 4, 8] {
        let mut query = query(&[], 0, 1);
        query.limits.max_concurrent_phase2_states = concurrency;
        let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
        let provider = Arc::new(DelayedMatrix::new(1, None));
        let street = MatrixCache::new(provider.clone());
        let evaluator = test_evaluator(&network, &retained, &snapshot, &query, &street, None);
        let run = Run::new(&evaluator);
        let states = (0..10)
            .map(|i| {
                let mut state = EvaluationState::initial(&query);
                state.ready_at += Duration::nanoseconds(i);
                state
            })
            .collect::<Vec<_>>();
        let successors = run.expand_edge(&states, &corridor, 0).await.unwrap();
        assert_eq!(run.counters.states.load(Ordering::Relaxed), 10);
        let ordered = successors
            .iter()
            .map(|s| s.movements.clone())
            .collect::<Vec<_>>();
        let pruned = prune_states(successors, 16)
            .into_iter()
            .map(|s| s.movements)
            .collect::<Vec<_>>();
        if let Some(expected) = &expected {
            assert_eq!(&(ordered, pruned), expected);
        } else {
            expected = Some((ordered, pruned));
        }
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        assert_eq!((street.requests(), street.hits()), (1, 9));
        assert_eq!(provider.peak.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn fatal_deadline_drops_outstanding_states() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    query.limits.max_concurrent_phase2_states = 4;
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let provider = Arc::new(DelayedMatrix::new(2000, None));
    let street = MatrixCache::new(provider.clone());
    let states = (0..10)
        .map(|i| {
            let mut s = EvaluationState::initial(&query);
            s.ready_at += Duration::seconds(i);
            s
        })
        .collect::<Vec<_>>();
    let evaluator = test_evaluator(&network, &retained, &snapshot, &query, &street, None);
    let run = Run::new(&evaluator);
    let outcome = run.expand_edge(&states, &corridor, 0).await;
    assert!(matches!(outcome, Err(RoutingError::DeadlineExceeded)));
    assert_eq!(run.counters.successors.load(Ordering::Relaxed), 0);
    assert_eq!(provider.active.load(Ordering::Relaxed), 0);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
}

fn real_bike_session(street: Arc<MatrixCache>, now: time::OffsetDateTime) -> BikeShareSession {
    use crate::domain::bikeshare::{
        BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
        BikeStationAvailability,
    };
    use crate::routing::evaluation::bike::BikeShareMovementPlanner;
    let pickup = BikeStationId::new("pickup");
    let dropoff = BikeStationId::new("dropoff");
    let stations = vec![
        BikeStation::new(
            pickup.clone(),
            "Pickup",
            GeoCoordinate {
                latitude: 25.0001,
                longitude: 121.5,
            },
        ),
        BikeStation::new(
            dropoff.clone(),
            "Dropoff",
            GeoCoordinate {
                latitude: 25.001,
                longitude: 121.5,
            },
        ),
    ];
    let links = ["origin", "a"]
        .into_iter()
        .map(|id| {
            BikeStationAccess::new(
                BoardingPointId::new(id),
                pickup.clone(),
                Some(BikeWalkingAccess::new(10, 10)),
                Some(BikeWalkingAccess::new(10, 10)),
            )
        })
        .chain(["destination", "b"].into_iter().map(|id| {
            BikeStationAccess::new(
                BoardingPointId::new(id),
                dropoff.clone(),
                Some(BikeWalkingAccess::new(10, 10)),
                Some(BikeWalkingAccess::new(10, 10)),
            )
        }))
        .collect();
    let network = Arc::new(BikeShareNetwork::new(stations).with_access_links(links));
    let snapshot = Arc::new(BikeAvailabilitySnapshot::new(
        now,
        HashMap::from([
            (
                pickup.clone(),
                BikeStationAvailability::new(pickup, 10, 0, true, now),
            ),
            (
                dropoff.clone(),
                BikeStationAvailability::new(dropoff, 0, 10, true, now),
            ),
        ]),
    ));
    BikeShareSession::new(
        BikeShareMovementPlanner::new(network, crate::config::ViaConfig::default().bike_policy()),
        snapshot,
        now,
        street,
    )
}

fn boarding_anchor(id: &str) -> CorridorAnchor {
    CorridorAnchor {
        kind: JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new(id),
        },
        coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
    }
}

#[tokio::test]
async fn real_bike_cache_deduplicates_concurrent_pairs_and_remaps_anchors() {
    use futures_util::{StreamExt, stream};
    let provider = Arc::new(DelayedMatrix::new(1, None));
    let matrix = Arc::new(MatrixCache::new(provider.clone()));
    let session = real_bike_session(matrix.clone(), datetime!(2026-09-21 10:00 UTC));
    let from = CorridorAnchor {
        kind: JourneyAnchorKind::Origin,
        coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
    };
    let to = CorridorAnchor {
        kind: JourneyAnchorKind::Destination,
        coordinate: Coordinate::try_new(25.002, 121.5).unwrap(),
    };
    let results = stream::iter(0..10)
        .map(|i| {
            let session = &session;
            let from = &from;
            let to = &to;
            async move { session.movements(from, to, i, i + 1).await.unwrap() }
        })
        .buffer_unordered(10)
        .collect::<Vec<_>>()
        .await;
    assert_eq!((session.requests(), session.hits()), (1, 9));
    assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
    let mut anchors = results
        .iter()
        .map(|movements| {
            assert_eq!(movements.len(), 1);
            assert_eq!(
                movements[0].total_seconds,
                720 + crate::config::ViaConfig::default()
                    .bikeshare
                    .rental_handling_seconds
            );
            (movements[0].from_anchor, movements[0].to_anchor)
        })
        .collect::<Vec<_>>();
    anchors.sort_unstable();
    assert_eq!(anchors, (0..10).map(|i| (i, i + 1)).collect::<Vec<_>>());
    let hits_before = matrix.hits();
    for ids in [("origin", "destination"), ("a", "b")] {
        let from = CorridorAnchor {
            coordinate: from.coordinate,
            ..boarding_anchor(ids.0)
        };
        let to = CorridorAnchor {
            coordinate: to.coordinate,
            ..boarding_anchor(ids.1)
        };
        assert_eq!(
            session.movements(&from, &to, 0, 1).await.unwrap()[0].total_seconds,
            140 + crate::config::ViaConfig::default()
                .bikeshare
                .rental_handling_seconds
        );
    }
    assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
    assert_eq!(matrix.hits() - hits_before, 2);
}

#[tokio::test]
async fn real_bike_failures_are_cached_with_their_classification() {
    for error in [
        StreetRoutingError::Timeout,
        StreetRoutingError::MalformedResponse,
        StreetRoutingError::Unreachable,
    ] {
        let provider = Arc::new(DelayedMatrix::new(1, Some(error.clone())));
        let matrix = Arc::new(MatrixCache::new(provider.clone()));
        let session = real_bike_session(matrix, datetime!(2026-09-21 10:00 UTC));
        let corridor = EvaluationCorridor::direct_walking(
            Coordinate::try_new(25.0, 121.5).unwrap(),
            Coordinate::try_new(25.002, 121.5).unwrap(),
        );
        let (from, to) = (&corridor.anchors()[0], &corridor.anchors()[1]);
        let (first, second) = tokio::join!(
            session.movements(from, to, 0, 1),
            session.movements(from, to, 0, 1)
        );
        assert_eq!(first, Err(error.clone()));
        assert_eq!(second, Err(error.clone()));
        assert_eq!(session.movements(from, to, 0, 1).await, Err(error));
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        assert_eq!(session.hits(), 2);
    }
}

#[tokio::test]
async fn mixed_and_bike_heavy_evaluations_keep_journeys_and_rental_continuations() {
    for variant in ["mixed", "bike"] {
        let reference = measured_case(1, None, 1, variant).await;
        assert!(reference.bicycle_calls > 0);
        for concurrency in [2, 4, 8] {
            let actual = measured_case(1, None, concurrency, variant).await;
            assert_eq!(actual.journeys, reference.journeys);
            assert_eq!(
                actual.stats.states_expanded,
                reference.stats.states_expanded
            );
            assert_eq!(
                actual.stats.successors_generated,
                reference.stats.successors_generated
            );
            assert_eq!(actual.calls, reference.calls);
            assert_eq!(actual.bicycle_calls, reference.bicycle_calls);
            assert!(actual.peak <= 8);
        }
    }
}

#[tokio::test]
async fn simultaneous_planning_requests_have_independent_caches() {
    use futures_util::{StreamExt, stream};
    let reference = measured_case(1, None, 4, "mixed").await;
    let results = stream::iter(0..4)
        .map(|_| measured_case(1, None, 4, "mixed"))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    for result in results {
        assert_eq!(result.journeys, reference.journeys);
        assert_eq!(result.calls, reference.calls);
        assert_eq!(
            (result.stats.matrix_requests, result.stats.matrix_cache_hits),
            (
                reference.stats.matrix_requests,
                reference.stats.matrix_cache_hits
            )
        );
    }
}

struct ReverseBikePlanner {
    started: Arc<tokio::sync::Semaphore>,
    completed: Arc<tokio::sync::Semaphore>,
    gates: Arc<Vec<tokio::sync::Semaphore>>,
}

#[async_trait::async_trait]
impl BikeMovementSource for ReverseBikePlanner {
    async fn movements(
        &self,
        _: &CorridorAnchor,
        _: &CorridorAnchor,
        from_anchor: usize,
        _: usize,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        self.started.add_permits(1);
        self.gates[from_anchor].acquire().await.unwrap().forget();
        self.completed.add_permits(1);
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn forced_reverse_completion_preserves_successor_order() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut reference = None;
    for concurrency in [1, 4] {
        let mut query = query(&[], 0, 1);
        query.limits.max_concurrent_phase2_states = concurrency;
        let direct = EvaluationCorridor::direct_walking(query.origin, query.destination);
        let anchors = vec![direct.anchors()[0].clone(); 6];
        let edges = (0..5).map(|_| PlannedMovement::WalkingToResolve).collect();
        let corridor = EvaluationCorridor::new(anchors, edges).unwrap();
        let street = MatrixCache::new(Arc::new(DelayedMatrix::new(0, None)));
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let completed = Arc::new(tokio::sync::Semaphore::new(0));
        let gates = Arc::new(
            (0..4)
                .map(|_| tokio::sync::Semaphore::new(usize::from(concurrency == 1)))
                .collect::<Vec<_>>(),
        );
        let bike = ReverseBikePlanner {
            started: started.clone(),
            completed: completed.clone(),
            gates: gates.clone(),
        };
        let evaluator =
            test_evaluator(&network, &retained, &snapshot, &query, &street, Some(&bike));
        let run = Run::new(&evaluator);
        let states = (0..4)
            .map(|index| {
                let mut state = EvaluationState::initial(&query);
                state.ready_at += Duration::seconds(index as i64);
                state.active_bike_rental = Some(super::super::state::ActiveBikeRental {
                    start_anchor: index,
                    started_at: query.departure_time,
                    prefix_movements: Vec::new(),
                    movement: bike_movement(index, 4, 60),
                });
                state
            })
            .collect::<Vec<_>>();
        let control = async {
            if concurrency == 4 {
                started.acquire_many(4).await.unwrap().forget();
                for index in (0..4).rev() {
                    gates[index].add_permits(1);
                    completed.acquire().await.unwrap().forget();
                }
            }
        };
        let (outcome, ()) = tokio::join!(run.expand_edge(&states, &corridor, 4), control);
        let successors = outcome.unwrap();
        let ordered = successors
            .iter()
            .map(|state| state.movements.clone())
            .collect::<Vec<_>>();
        let frontier = prune_states(successors, 16)
            .into_iter()
            .map(|state| state.movements)
            .collect::<Vec<_>>();
        if let Some(reference) = &reference {
            assert_eq!(&(ordered, frontier), reference);
        } else {
            reference = Some((ordered, frontier));
        }
    }
}

#[tokio::test]
async fn different_state_times_share_inflight_walking_and_keep_arrivals() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[], 0, 1);
    query.limits.max_concurrent_phase2_states = 4;
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let mut provider = DelayedMatrix::new(0, None);
    provider.gate = Some(gate.clone());
    provider.started = Some(started.clone());
    let provider = Arc::new(provider);
    let street = MatrixCache::new(provider.clone());
    let evaluator = test_evaluator(&network, &retained, &snapshot, &query, &street, None);
    let run = Run::new(&evaluator);
    let states = [0, 1, 180, 3600].map(|seconds| {
        let mut state = EvaluationState::initial(&query);
        state.ready_at += Duration::seconds(seconds);
        state
    });
    let control = async {
        started.acquire().await.unwrap().forget();
        tokio::task::yield_now().await;
        gate.add_permits(1);
    };
    let (outcome, ()) = tokio::join!(run.expand_edge(&states, &corridor, 0), control);
    let successors = outcome.unwrap();
    assert_eq!(successors.len(), states.len());
    for (state, successor) in states.iter().zip(successors) {
        assert_eq!(successor.ready_at, state.ready_at + Duration::seconds(300));
    }
    assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    assert_eq!((street.requests(), street.hits()), (1, 3));
}

#[tokio::test]
async fn distinct_bike_movement_keys_share_one_inflight_bicycle_query() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let mut controlled = DelayedMatrix::new(0, None);
    controlled.gate = Some(gate.clone());
    controlled.started = Some(started.clone());
    let provider = Arc::new(controlled);
    let matrix = Arc::new(MatrixCache::new(provider.clone()));
    let session = real_bike_session(matrix.clone(), datetime!(2026-09-21 10:00 UTC));
    let (from1, to1, from2, to2) = (
        boarding_anchor("origin"),
        boarding_anchor("destination"),
        boarding_anchor("a"),
        boarding_anchor("b"),
    );
    let work = async {
        tokio::join!(
            session.movements(&from1, &to1, 1, 2),
            session.movements(&from2, &to2, 3, 4),
            async {
                started.acquire().await.unwrap().forget();
                tokio::task::yield_now().await;
                gate.add_permits(1);
            }
        )
    };
    let (a, b, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), work)
        .await
        .unwrap();
    assert_eq!(a.unwrap()[0].from_anchor, 1);
    assert_eq!(b.unwrap()[0].from_anchor, 3);
    assert_eq!(session.requests(), 2);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    assert_eq!((matrix.requests(), matrix.hits()), (1, 1));
}

struct FailOnceMatrix(AtomicUsize);
#[async_trait::async_trait]
impl StreetRouter for FailOnceMatrix {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
            return Err(StreetRoutingError::Timeout);
        }
        Ok(MatrixResult {
            cells: vec![
                vec![
                    MatrixCell::Reachable {
                        duration: Seconds(120),
                        distance: Metres(1000)
                    };
                    query.targets.len()
                ];
                query.sources.len()
            ],
        })
    }
    async fn route(&self, _: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        panic!("geometry")
    }
}

#[tokio::test]
async fn cross_anchor_transient_retry_is_suppressed_only_within_the_shared_request() {
    let provider = Arc::new(FailOnceMatrix(AtomicUsize::new(0)));
    let session = real_bike_session(
        Arc::new(MatrixCache::new(provider.clone())),
        datetime!(2026-09-21 10:00 UTC),
    );
    for (from, to) in [("origin", "destination"), ("a", "b")] {
        let result = session
            .movements(&boarding_anchor(from), &boarding_anchor(to), 0, 1)
            .await;
        assert_eq!(result, Err(StreetRoutingError::Timeout));
    }
    assert_eq!(provider.0.load(Ordering::Relaxed), 1);
    let fresh = real_bike_session(
        Arc::new(MatrixCache::new(provider.clone())),
        datetime!(2026-09-21 10:00 UTC),
    );
    assert!(
        !fresh
            .movements(
                &boarding_anchor("origin"),
                &boarding_anchor("destination"),
                0,
                1
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(provider.0.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn corridor_concurrency_preserves_transit_and_mixed_results_and_shares_caches() {
    for variant in ["walking", "mixed"] {
        let reference = measured_corridors_case(5, None, 4, 1, 8, variant).await;
        assert!(!reference.journeys.is_empty());
        for concurrency in [2, 4, 8] {
            let actual = measured_corridors_case(5, None, 4, concurrency, 8, variant).await;
            assert_eq!(actual.journeys, reference.journeys);
            assert_eq!(
                actual.stats.states_expanded,
                reference.stats.states_expanded
            );
            assert_eq!(
                actual.stats.successors_generated,
                reference.stats.successors_generated
            );
            assert_eq!(actual.stats.corridors_evaluated, 8);
            assert_eq!(actual.calls, reference.calls);
            assert!(actual.stats.matrix_cache_hits > 0);
            assert!(actual.peak <= 8);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn corridor_deadline_cancels_pending_provider_operations() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    query.limits.max_concurrent_phase2_corridors = 4;
    let provider = Arc::new(DelayedMatrix::new(2000, None));
    let street = MatrixCache::new(provider.clone());
    let corridors = (0..8)
        .map(|i| {
            EvaluationCorridor::direct_walking(
                Coordinate::try_new(25.0 + i as f64 / 1000.0, 121.5).unwrap(),
                Coordinate::try_new(25.1, 121.5).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let report = evaluate_corridors(
        &network, &retained, &snapshot, &query, &street, None, &corridors,
    )
    .await;
    assert!(matches!(report, Err(RoutingError::DeadlineExceeded)));
    assert_eq!(provider.active.load(Ordering::Relaxed), 0);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 4);
}

struct OutOfOrderMatrix {
    started: std::sync::Mutex<Vec<usize>>,
    completed: std::sync::Mutex<Vec<usize>>,
}

#[async_trait::async_trait]
impl StreetRouter for OutOfOrderMatrix {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        let index = ((query.sources[0].latitude() - 25.0) * 1000.0).round() as usize;
        self.started.lock().unwrap().push(index);
        tokio::time::sleep(std::time::Duration::from_millis((8 - index) as u64 * 5)).await;
        self.completed.lock().unwrap().push(index);
        Ok(MatrixResult {
            cells: vec![vec![MatrixCell::Reachable {
                duration: Seconds(300 + index as u32),
                distance: Metres(1000),
            }]],
        })
    }
    async fn route(&self, _: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        unreachable!()
    }
}

#[tokio::test(start_paused = true)]
async fn corridors_start_in_order_and_overlap_without_reordering_journeys() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut reference = None;
    for concurrency in [1, 2, 4, 8] {
        let mut query = query(&[], 0, 1);
        query.limits.max_concurrent_phase2_corridors = concurrency;
        let corridors = (0..8)
            .map(|i| {
                EvaluationCorridor::direct_walking(
                    Coordinate::try_new(25.0 + i as f64 / 1000.0, 121.5).unwrap(),
                    Coordinate::try_new(25.1, 121.5).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let provider = Arc::new(OutOfOrderMatrix {
            started: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(Vec::new()),
        });
        let street = MatrixCache::new(provider.clone());
        let result = evaluate_corridors(
            &network, &retained, &snapshot, &query, &street, None, &corridors,
        )
        .await
        .unwrap();
        let signatures = result
            .journeys
            .iter()
            .map(|j| j.signature())
            .collect::<Vec<_>>();
        if let Some(expected) = &reference {
            assert_eq!(&signatures, expected);
        } else {
            reference = Some(signatures);
        }
        assert_eq!(
            *provider.started.lock().unwrap(),
            (0..8).collect::<Vec<_>>()
        );
        if concurrency == 8 {
            assert_ne!(provider.completed.lock().unwrap()[0], 0);
            assert_ne!(
                *provider.completed.lock().unwrap(),
                (0..8).collect::<Vec<_>>()
            );
        }
        assert_eq!(result.stats.corridors_evaluated, 8);
        assert_eq!(result.stats.states_expanded, 8);
    }
}

#[tokio::test]
async fn corridor_provider_failures_keep_classification_and_reuse_cache() {
    for (error, transient, internal) in [
        (StreetRoutingError::Timeout, true, false),
        (StreetRoutingError::MalformedResponse, false, true),
        (StreetRoutingError::Unreachable, false, false),
    ] {
        let network = network(Vec::new(), &[], Vec::new());
        let retained = RetainedCorridorSet::default();
        let snapshot = TimingSnapshot::default();
        let mut query = query(&[], 0, 1);
        query.limits.max_concurrent_phase2_corridors = 8;
        let provider = Arc::new(DelayedMatrix::new(5, Some(error)));
        let street = MatrixCache::new(provider.clone());
        let corridors = (0..8)
            .map(|_| EvaluationCorridor::direct_walking(query.origin, query.destination))
            .collect::<Vec<_>>();
        let report = evaluate_corridors(
            &network, &retained, &snapshot, &query, &street, None, &corridors,
        )
        .await
        .unwrap();
        assert!(report.journeys.is_empty());
        assert_eq!(
            (report.failures.transient, report.failures.internal),
            (transient, internal)
        );
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        assert_eq!(report.stats.corridors_evaluated, 8);
    }
}

struct FatalRaceMatrix {
    calls: AtomicUsize,
    active: AtomicUsize,
}

#[async_trait::async_trait]
impl StreetRouter for FatalRaceMatrix {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        let index = ((query.sources[0].latitude() - 25.0) * 1000.0).round() as usize;
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
        let _active = Active(&self.active);
        tokio::time::sleep(std::time::Duration::from_millis(match index {
            0 => 40,
            1 => 10,
            _ => 500,
        }))
        .await;
        Ok(MatrixResult {
            cells: vec![vec![MatrixCell::Reachable {
                duration: Seconds(1),
                distance: Metres(1),
            }]],
        })
    }
    async fn route(&self, _: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        unreachable!()
    }
}

#[tokio::test]
async fn fatal_corridor_race_returns_first_observed_error_and_cancels_pending_work() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 1);
    query.limits.max_concurrent_phase2_corridors = 3;
    let retained = candidate_corridor(&network, &query);
    let leg = retained.retained()[0]
        .transit_legs()
        .next()
        .unwrap()
        .clone();
    let corridor = |index: usize, invalid_alight: usize| {
        let direct = EvaluationCorridor::direct_walking(
            Coordinate::try_new(25.0 + index as f64 / 1000.0, 121.5).unwrap(),
            Coordinate::try_new(25.1, 121.5).unwrap(),
        );
        let mut bad_leg = leg.clone();
        bad_leg.alight_index = invalid_alight;
        EvaluationCorridor::new(
            vec![
                direct.anchors()[0].clone(),
                direct.anchors()[1].clone(),
                direct.anchors()[1].clone(),
            ],
            vec![
                PlannedMovement::WalkingToResolve,
                PlannedMovement::Transit(bad_leg),
            ],
        )
        .unwrap()
    };
    let corridors = vec![
        corridor(0, 0),
        corridor(1, 999),
        corridor(2, 0),
        corridor(3, 0),
    ];
    let snapshot = TimingSnapshot::default();
    let provider = Arc::new(FatalRaceMatrix {
        calls: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
    });
    let street = MatrixCache::new(provider.clone());
    let report = evaluate_corridors(
        &network, &retained, &snapshot, &query, &street, None, &corridors,
    )
    .await;
    assert!(
        matches!(report, Err(RoutingError::Internal(message)) if message.contains("invalid alighting index"))
    );
    assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
    assert_eq!(provider.active.load(Ordering::Relaxed), 0);
}
