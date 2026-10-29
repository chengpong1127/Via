use super::*;
use crate::street::*;
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

async fn measured_case(
    delay: u64,
    error: Option<StreetRoutingError>,
    concurrency: usize,
    variant: &str,
) -> (
    u64,
    usize,
    usize,
    Vec<String>,
    Phase2EvaluationStats,
    u64,
    usize,
) {
    measured_corridors_case(delay, error, concurrency, 1, 1, variant).await
}

async fn measured_corridors_case(
    delay: u64,
    error: Option<StreetRoutingError>,
    concurrency: usize,
    corridor_concurrency: usize,
    corridor_count: usize,
    variant: &str,
) -> (
    u64,
    usize,
    usize,
    Vec<String>,
    Phase2EvaluationStats,
    u64,
    usize,
) {
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
                    CorridorEdge {
                        planned_movement: PlannedMovement::RetainedWalking {
                            kind: WalkingMovementKind::Access,
                            duration_seconds: 0,
                            distance_metres: 0,
                            access_available_at: Some(query.departure_time),
                        },
                    },
                    CorridorEdge {
                        planned_movement: PlannedMovement::Transit(
                            retained.retained()[0]
                                .transit_legs()
                                .next()
                                .unwrap()
                                .clone(),
                        ),
                    },
                    CorridorEdge {
                        planned_movement: PlannedMovement::WalkingToResolve,
                    },
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
    let matrix = Arc::new(crate::routing::matrix::SharedMatrixResolver::new(
        provider.clone(),
    ));
    let walking = WalkingCache::with_resolver(matrix.clone());
    let bike = (variant != "walking").then(|| real_bike_session(matrix, query.departure_time));
    if variant == "bike" {
        snapshot = TimingSnapshot::default();
    }
    let result = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &walking,
        bike.as_ref(),
    )
    .evaluate(&corridors)
    .await
    .unwrap();
    (
        result.stats.phase2_latency_us,
        provider.calls.load(Ordering::Relaxed),
        provider.peak.load(Ordering::Relaxed),
        result.journeys.iter().map(|j| format!("{:?}", j)).collect(),
        result.stats,
        provider.waiting_us.load(Ordering::Relaxed),
        provider.bicycle_calls.load(Ordering::Relaxed),
    )
}

#[tokio::test]
#[ignore = "Phase B controlled asynchronous provider comparison"]
async fn phase_b_io_benchmark() {
    let concurrency = std::env::var("VIA_PHASE_B_CONCURRENCY")
        .ok()
        .map(|v| v.parse().unwrap())
        .unwrap_or(4);
    let variant = std::env::var("VIA_PHASE_B_VARIANT").unwrap_or_else(|_| "walking".into());
    let variant = variant.as_str();
    for delay in [0, 10, 50, 100] {
        for (label, error) in [
            ("success", None),
            ("unreachable", Some(StreetRoutingError::Unreachable)),
            ("timeout", Some(StreetRoutingError::Timeout)),
        ] {
            let mut samples = Vec::new();
            let mut expected = None;
            let mut last = None;
            for iteration in 0..22 {
                let result = measured_case(delay, error.clone(), concurrency, variant).await;
                if let Some(expected) = &expected {
                    assert_eq!(&result.3, expected);
                } else {
                    expected = Some(result.3.clone());
                }
                if iteration >= 2 {
                    samples.push(result.0);
                }
                last = Some(result);
            }
            samples.sort_unstable();
            let last = last.unwrap();
            println!(
                "phase_b variant={variant} concurrency={concurrency} delay={delay} case={label} p50_us={} p95_us={} worst_us={} physical={} peak={} successors={} candidates={} logical={} completed_hits={} inflight_hits={} misses={} walking_requests={} walking_hits={} bike_hits={} wait_us={} bicycle_calls={} fingerprints={}",
                samples[10],
                samples[18],
                samples[19],
                last.1,
                last.2,
                last.4.successors_generated,
                last.4.final_candidates,
                last.4.matrix.logical_requests,
                last.4.matrix.completed_hits,
                last.4.matrix.inflight_hits,
                last.4.matrix.misses,
                last.4.walking_matrix_requests,
                last.4.walking_cache_hits,
                last.4.bike_exact_cache_hits,
                last.5,
                last.6,
                fingerprint(&expected)
            );
        }
    }
}

#[tokio::test]
async fn successful_evaluations_preserve_ordered_journeys_and_work_at_all_limits() {
    let reference = measured_case(1, None, 1, "walking").await;
    for concurrency in [2, 4, 8] {
        let actual = measured_case(1, None, concurrency, "walking").await;
        assert_eq!(actual.3, reference.3);
        assert_eq!(actual.4.states_expanded, reference.4.states_expanded);
        assert_eq!(
            actual.4.successors_generated,
            reference.4.successors_generated
        );
        assert_eq!(
            actual.4.corridors_completed,
            reference.4.corridors_completed
        );
        assert_eq!(actual.4.final_candidates, reference.4.final_candidates);
        assert_eq!(actual.1, reference.1);
        assert!(actual.2 >= 1);
        assert!(actual.2 <= concurrency);
        assert_eq!(actual.4.matrix.physical_requests, actual.1);
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
        let walking = WalkingCache::new(provider.clone());
        let resolver = expansion::ExpansionResolver {
            inputs: expansion_inputs(&network, &retained, &snapshot, &query),
            walking: &walking,
            bike_share: None,
        };
        let states = (0..10)
            .map(|i| {
                let mut state = EvaluationState::initial(&query);
                state.ready_at += Duration::nanoseconds(i);
                state
            })
            .collect::<Vec<_>>();
        let outcome = resolver
            .expand_edge(&states, &corridor, &corridor.edges()[0], 0)
            .await;
        assert_eq!(outcome.stats.states_expanded, 10);
        let successors = outcome.successors.unwrap();
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
        assert_eq!(walking.requests(), 1);
        assert_eq!(walking.cache_hits(), 9);
        assert_eq!(provider.peak.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn fatal_deadline_drops_outstanding_states_and_keeps_observed_outcomes() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    query.limits.max_concurrent_phase2_states = 4;
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let provider = Arc::new(DelayedMatrix::new(2000, None));
    let walking = WalkingCache::new(provider.clone());
    let states = (0..10)
        .map(|i| {
            let mut s = EvaluationState::initial(&query);
            s.ready_at += Duration::seconds(i);
            s
        })
        .collect::<Vec<_>>();
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &walking,
        bike_share: None,
    };
    let outcome = resolver
        .expand_edge(&states, &corridor, &corridor.edges()[0], 0)
        .await;
    assert!(matches!(
        outcome.successors,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(outcome.stats.states_expanded, 0);
    assert_eq!(outcome.stats.successors_generated, 0);
    assert_eq!(provider.active.load(Ordering::Relaxed), 0);
    assert_eq!(walking.matrix_stats().canceled_requests, 1);
}

fn real_bike_session(street: Arc<dyn StreetRouter>, now: time::OffsetDateTime) -> BikeShareSession {
    use crate::domain::bikeshare::{
        BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
        BikeStationAvailability,
    };
    use crate::routing::bike::BikeShareMovementPlanner;
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
        BikeShareMovementPlanner::new(
            network,
            Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
            street,
            crate::config::ViaConfig::default().bike_policy(),
        ),
        snapshot,
        now,
    )
}

#[tokio::test]
async fn real_bike_cache_deduplicates_concurrent_pairs_and_remaps_anchors() {
    use crate::routing::corridor::CorridorAnchor;
    use futures_util::{StreamExt, stream};
    let provider = Arc::new(DelayedMatrix::new(1, None));
    let matrix = Arc::new(crate::routing::matrix::SharedMatrixResolver::new(
        provider.clone(),
    ));
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
            async move {
                let request = BikeMovementRequest {
                    from,
                    to,
                    from_anchor: i,
                    to_anchor: i + 1,
                };
                let candidate = session.candidate(&request).unwrap();
                session.realize(&request, &candidate).await.unwrap()
            }
        })
        .buffer_unordered(10)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(session.realization_stats().exact_requests, 1);
    assert_eq!(session.realization_stats().cache_hits, 9);
    assert_eq!(session.realization_stats().matrix_requests, 3);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
    let mut anchors = results
        .iter()
        .map(|movements| {
            assert_eq!(movements.len(), 1);
            assert_eq!(movements[0].total_seconds, 720);
            (movements[0].from_anchor, movements[0].to_anchor)
        })
        .collect::<Vec<_>>();
    anchors.sort_unstable();
    assert_eq!(anchors, (0..10).map(|i| (i, i + 1)).collect::<Vec<_>>());
    for ids in [("origin", "destination"), ("a", "b")] {
        let from = CorridorAnchor {
            kind: JourneyAnchorKind::BoardingPoint {
                id: BoardingPointId::new(ids.0),
            },
            ..from.clone()
        };
        let to = CorridorAnchor {
            kind: JourneyAnchorKind::BoardingPoint {
                id: BoardingPointId::new(ids.1),
            },
            ..to.clone()
        };
        let request = BikeMovementRequest {
            from: &from,
            to: &to,
            from_anchor: 0,
            to_anchor: 1,
        };
        let candidate = session.candidate(&request).unwrap();
        assert_eq!(
            session.realize(&request, &candidate).await.unwrap()[0].total_seconds,
            140
        );
    }
    assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
    assert_eq!(matrix.stats().completed_hits, 2);
}

#[tokio::test]
async fn real_bike_failures_are_cached_with_their_classification() {
    for error in [
        StreetRoutingError::Timeout,
        StreetRoutingError::MalformedResponse,
        StreetRoutingError::Unreachable,
    ] {
        let provider = Arc::new(DelayedMatrix::new(1, Some(error.clone())));
        let matrix = Arc::new(crate::routing::matrix::SharedMatrixResolver::new(
            provider.clone(),
        ));
        let session = real_bike_session(matrix, datetime!(2026-09-21 10:00 UTC));
        let corridor = EvaluationCorridor::direct_walking(
            Coordinate::try_new(25.0, 121.5).unwrap(),
            Coordinate::try_new(25.002, 121.5).unwrap(),
        );
        let request = BikeMovementRequest {
            from: &corridor.anchors()[0],
            to: &corridor.anchors()[1],
            from_anchor: 0,
            to_anchor: 1,
        };
        let candidate = session.candidate(&request).unwrap();
        let (first, second) = tokio::join!(
            session.realize(&request, &candidate),
            session.realize(&request, &candidate)
        );
        assert_eq!(first, Err(error.clone()));
        assert_eq!(second, Err(error.clone()));
        assert_eq!(session.realize(&request, &candidate).await, Err(error));
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        assert_eq!(session.realization_stats().cache_hits, 2);
    }
}

fn fingerprint(value: &impl std::fmt::Debug) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(format!("{value:?}").as_bytes()))
}

#[tokio::test]
async fn mixed_and_bike_heavy_evaluations_keep_journeys_and_rental_continuations() {
    for variant in ["mixed", "bike"] {
        let reference = measured_case(1, None, 1, variant).await;
        assert!(reference.6 > 0);
        for concurrency in [2, 4, 8] {
            let actual = measured_case(1, None, concurrency, variant).await;
            assert_eq!(actual.3, reference.3);
            assert_eq!(actual.4.states_expanded, reference.4.states_expanded);
            assert_eq!(
                actual.4.successors_generated,
                reference.4.successors_generated
            );
            assert_eq!(actual.1, reference.1);
            assert_eq!(actual.6, reference.6);
            assert!(actual.2 <= 8);
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
        assert_eq!(result.3, reference.3);
        assert_eq!(result.1, reference.1);
        assert_eq!(result.4.matrix, reference.4.matrix);
    }
}

#[tokio::test]
#[ignore = "Phase B exact-query reuse and provider semaphore comparison"]
async fn phase_b_matrix_benchmark() {
    use futures_util::{StreamExt, stream};
    let concurrency: usize = std::env::var("VIA_PHASE_B_CONCURRENCY")
        .ok()
        .map(|v| v.parse().unwrap())
        .unwrap_or(4);
    let raw = std::env::var_os("VIA_PHASE_B_RAW_MATRIX").is_some();
    for delay in [0, 10, 50, 100] {
        for identical in [false, true] {
            for requests in [1, 4] {
                let mut samples = Vec::new();
                let mut last = (0, 0, 0, 0, 0);
                for iteration in 0..22 {
                    let provider = Arc::new(DelayedMatrix::new(delay, None));
                    let started = std::time::Instant::now();
                    let totals = stream::iter(0..requests)
                        .map(|_| {
                            let provider = provider.clone();
                            async move {
                                let matrix =
                                    Arc::new(crate::routing::matrix::SharedMatrixResolver::new(
                                        provider.clone(),
                                    ));
                                let router: Arc<dyn StreetRouter> =
                                    if raw { provider } else { matrix.clone() };
                                let results = stream::iter(0..10)
                                    .map(|index| {
                                        let router = router.clone();
                                        async move {
                                            router
                                                .matrix(MatrixQuery {
                                                    orientation: MatrixOrientation::OneToMany,
                                                    travel_mode: StreetTravelMode::Walking,
                                                    sources: vec![
                                                        Coordinate::try_new(25.0, 121.5).unwrap(),
                                                    ],
                                                    targets: vec![
                                                        Coordinate::try_new(
                                                            25.001
                                                                + if identical {
                                                                    0.0
                                                                } else {
                                                                    f64::from(index) / 1000.0
                                                                },
                                                            121.5,
                                                        )
                                                        .unwrap(),
                                                    ],
                                                    constraints: RoutingConstraints::default(),
                                                })
                                                .await
                                                .unwrap()
                                        }
                                    })
                                    .buffer_unordered(concurrency)
                                    .collect::<Vec<_>>()
                                    .await;
                                assert_eq!(results.len(), 10);
                                matrix.stats()
                            }
                        })
                        .buffer_unordered(requests)
                        .collect::<Vec<_>>()
                        .await;
                    let elapsed = started.elapsed().as_micros();
                    if iteration >= 2 {
                        samples.push(elapsed);
                    }
                    last = (
                        provider.calls.load(Ordering::Relaxed),
                        provider.peak.load(Ordering::Relaxed),
                        totals.iter().map(|s| s.completed_hits).sum(),
                        totals.iter().map(|s| s.inflight_hits).sum(),
                        provider.waiting_us.load(Ordering::Relaxed),
                    );
                    assert!(last.1 <= 8);
                    assert_eq!(last.0, requests * if identical && !raw { 1 } else { 10 });
                }
                samples.sort_unstable();
                println!(
                    "matrix raw={raw} concurrency={concurrency} delay={delay} identical={identical} requests={requests} p50_us={} p95_us={} worst_us={} logical={} physical={} peak={} completed_hits={} inflight_hits={} wait_us={}",
                    samples[10],
                    samples[18],
                    samples[19],
                    requests * 10,
                    last.0,
                    last.1,
                    last.2,
                    last.3,
                    last.4
                );
            }
        }
    }
}

struct ReverseBikePlanner {
    started: Arc<tokio::sync::Semaphore>,
    completed: Arc<tokio::sync::Semaphore>,
    gates: Arc<Vec<tokio::sync::Semaphore>>,
}

#[async_trait::async_trait]
impl TestBikeProvider for ReverseBikePlanner {
    fn has_candidate(&self, _: &BikeMovementRequest<'_>) -> bool {
        true
    }

    async fn realize(
        &self,
        request: &BikeMovementRequest<'_>,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        self.started.add_permits(1);
        self.gates[request.from_anchor]
            .acquire()
            .await
            .unwrap()
            .forget();
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
        let edges = (0..5)
            .map(|_| CorridorEdge {
                planned_movement: PlannedMovement::WalkingToResolve,
            })
            .collect();
        let corridor = EvaluationCorridor::new(anchors, edges).unwrap();
        let walking = WalkingCache::new(Arc::new(DelayedMatrix::new(0, None)));
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let completed = Arc::new(tokio::sync::Semaphore::new(0));
        let gates = Arc::new(
            (0..4)
                .map(|_| tokio::sync::Semaphore::new(usize::from(concurrency == 1)))
                .collect::<Vec<_>>(),
        );
        let bike = BikeShareSession::for_test(Box::new(ReverseBikePlanner {
            started: started.clone(),
            completed: completed.clone(),
            gates: gates.clone(),
        }));
        let resolver = expansion::ExpansionResolver {
            inputs: expansion_inputs(&network, &retained, &snapshot, &query),
            walking: &walking,
            bike_share: Some(&bike),
        };
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
        let (outcome, ()) = tokio::join!(
            resolver.expand_edge(&states, &corridor, &corridor.edges()[4], 4),
            control
        );
        let successors = outcome.successors.unwrap();
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
    let walking = WalkingCache::new(provider.clone());
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &walking,
        bike_share: None,
    };
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
    let (outcome, ()) = tokio::join!(
        resolver.expand_edge(&states, &corridor, &corridor.edges()[0], 0),
        control
    );
    let successors = outcome.successors.unwrap();
    assert_eq!(successors.len(), states.len());
    for (state, successor) in states.iter().zip(successors) {
        assert_eq!(successor.ready_at, state.ready_at + Duration::seconds(300));
    }
    assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    assert_eq!((walking.requests(), walking.cache_hits()), (1, 3));
}

#[tokio::test]
async fn distinct_bike_movement_keys_share_one_inflight_bicycle_query() {
    use crate::routing::corridor::CorridorAnchor;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let mut controlled = DelayedMatrix::new(0, None);
    controlled.gate = Some(gate.clone());
    controlled.started = Some(started.clone());
    let provider = Arc::new(controlled);
    let matrix = Arc::new(crate::routing::matrix::SharedMatrixResolver::new(
        provider.clone(),
    ));
    let session = real_bike_session(matrix.clone(), datetime!(2026-09-21 10:00 UTC));
    let anchor = |id: &str| CorridorAnchor {
        kind: JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new(id),
        },
        coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
    };
    let (from1, to1, from2, to2) = (
        anchor("origin"),
        anchor("destination"),
        anchor("a"),
        anchor("b"),
    );
    let first = BikeMovementRequest {
        from: &from1,
        to: &to1,
        from_anchor: 1,
        to_anchor: 2,
    };
    let second = BikeMovementRequest {
        from: &from2,
        to: &to2,
        from_anchor: 3,
        to_anchor: 4,
    };
    let a = session.candidate(&first).unwrap();
    let b = session.candidate(&second).unwrap();
    let work = async {
        tokio::join!(
            session.realize(&first, &a),
            session.realize(&second, &b),
            async {
                started.acquire().await.unwrap().forget();
                assert_eq!(matrix.stats().inflight_hits, 1);
                gate.add_permits(1);
            }
        )
    };
    let (a, b, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), work)
        .await
        .unwrap();
    assert_eq!(a.unwrap()[0].from_anchor, 1);
    assert_eq!(b.unwrap()[0].from_anchor, 3);
    assert_eq!(session.realization_stats().exact_requests, 2);
    assert_eq!(session.realization_stats().matrix_requests, 2);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    assert_eq!(matrix.stats().inflight_hits, 1);
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
    use crate::routing::corridor::CorridorAnchor;
    for shared in [false, true] {
        let provider = Arc::new(FailOnceMatrix(AtomicUsize::new(0)));
        let router: Arc<dyn StreetRouter> = if shared {
            Arc::new(crate::routing::matrix::SharedMatrixResolver::new(
                provider.clone(),
            ))
        } else {
            provider.clone()
        };
        let session = real_bike_session(router, datetime!(2026-09-21 10:00 UTC));
        for (index, (from, to)) in [("origin", "destination"), ("a", "b")]
            .into_iter()
            .enumerate()
        {
            let anchor = |id: &str| CorridorAnchor {
                kind: JourneyAnchorKind::BoardingPoint {
                    id: BoardingPointId::new(id),
                },
                coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
            };
            let (from, to) = (anchor(from), anchor(to));
            let request = BikeMovementRequest {
                from: &from,
                to: &to,
                from_anchor: 0,
                to_anchor: 1,
            };
            let candidate = session.candidate(&request).unwrap();
            let result = session.realize(&request, &candidate).await;
            if index == 0 || shared {
                assert_eq!(result, Err(StreetRoutingError::Timeout));
            } else {
                assert!(!result.unwrap().is_empty());
            }
        }
        assert_eq!(
            provider.0.load(Ordering::Relaxed),
            if shared { 1 } else { 2 }
        );
        let fresh = crate::routing::matrix::SharedMatrixResolver::new(provider);
        assert!(
            fresh
                .matrix(MatrixQuery {
                    orientation: MatrixOrientation::ManyToMany,
                    travel_mode: StreetTravelMode::Bicycle,
                    sources: vec![Coordinate::try_new(25.0001, 121.5).unwrap()],
                    targets: vec![Coordinate::try_new(25.001, 121.5).unwrap()],
                    constraints: RoutingConstraints::default(),
                })
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn corridor_concurrency_preserves_transit_and_mixed_results_and_shares_caches() {
    for variant in ["walking", "mixed"] {
        let reference = measured_corridors_case(5, None, 4, 1, 8, variant).await;
        assert!(!reference.3.is_empty());
        for concurrency in [2, 4, 8] {
            let actual = measured_corridors_case(5, None, 4, concurrency, 8, variant).await;
            assert_eq!(actual.3, reference.3);
            assert_eq!(actual.4.states_expanded, reference.4.states_expanded);
            assert_eq!(
                actual.4.successors_generated,
                reference.4.successors_generated
            );
            assert_eq!(actual.4.corridors_completed, 8);
            assert_eq!(actual.1, reference.1);
            assert!(actual.4.walking_cache_hits > 0);
            assert!(actual.2 <= 8);
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
    let walking = WalkingCache::new(provider.clone());
    let corridors = (0..8)
        .map(|i| {
            EvaluationCorridor::direct_walking(
                Coordinate::try_new(25.0 + i as f64 / 1000.0, 121.5).unwrap(),
                Coordinate::try_new(25.1, 121.5).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut evaluator =
        FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None);
    assert!(matches!(
        evaluator.evaluate(&corridors).await,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(provider.active.load(Ordering::Relaxed), 0);
    assert_eq!(evaluator.stats.corridors_attempted, 4);
}

struct OutOfOrderMatrix {
    completed: std::sync::Mutex<Vec<usize>>,
}

#[async_trait::async_trait]
impl StreetRouter for OutOfOrderMatrix {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        let index = ((query.sources[0].latitude() - 25.0) * 1000.0).round() as usize;
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

#[tokio::test]
async fn out_of_order_corridor_completion_preserves_ordered_journeys() {
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
            completed: std::sync::Mutex::new(Vec::new()),
        });
        let walking = WalkingCache::new(provider.clone());
        let result =
            FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None)
                .evaluate(&corridors)
                .await
                .unwrap();
        let identities = result
            .journeys
            .iter()
            .map(|j| j.identity().canonical_signature())
            .collect::<Vec<_>>();
        if let Some(expected) = &reference {
            assert_eq!(&identities, expected);
        } else {
            reference = Some(identities);
        }
        if concurrency == 8 {
            assert_ne!(
                *provider.completed.lock().unwrap(),
                (0..8).collect::<Vec<_>>()
            );
        }
        assert_eq!(result.stats.corridors_completed, 8);
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
        let walking = WalkingCache::new(provider.clone());
        let corridors = (0..8)
            .map(|_| EvaluationCorridor::direct_walking(query.origin, query.destination))
            .collect::<Vec<_>>();
        let mut evaluator =
            FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None);
        let result = evaluator.evaluate(&corridors).await.unwrap();
        assert!(result.journeys.is_empty());
        assert_eq!(
            (result.failures.transient, result.failures.internal),
            (transient, internal)
        );
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        assert_eq!(result.stats.corridors_completed, 8);
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
async fn fatal_corridor_race_prefers_earliest_input_and_cancels_later_work() {
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
                CorridorEdge {
                    planned_movement: PlannedMovement::WalkingToResolve,
                },
                CorridorEdge {
                    planned_movement: PlannedMovement::Transit(bad_leg),
                },
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
    let walking = WalkingCache::new(provider.clone());
    let mut evaluator =
        FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None);
    let result = evaluator.evaluate(&corridors).await;
    assert!(
        matches!(result, Err(RoutingError::Internal(message)) if message.contains("reversed stop indexes"))
    );
    assert_eq!(evaluator.stats.corridors_attempted, 3);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 3);
    assert_eq!(provider.active.load(Ordering::Relaxed), 0);
}

#[tokio::test(start_paused = true)]
async fn corridor_deadline_preserves_completed_edge_statistics() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    let direct = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let corridor = EvaluationCorridor::new(
        vec![
            direct.anchors()[0].clone(),
            direct.anchors()[1].clone(),
            direct.anchors()[1].clone(),
        ],
        vec![
            CorridorEdge {
                planned_movement: PlannedMovement::RetainedWalking {
                    kind: WalkingMovementKind::Access,
                    duration_seconds: 1,
                    distance_metres: 1,
                    access_available_at: None,
                },
            },
            CorridorEdge {
                planned_movement: PlannedMovement::WalkingToResolve,
            },
        ],
    )
    .unwrap();
    let provider = Arc::new(DelayedMatrix::new(2000, None));
    let walking = WalkingCache::new(provider.clone());
    let mut evaluator =
        FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None);
    assert!(matches!(
        evaluator.evaluate(&[corridor]).await,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(evaluator.stats.states_expanded, 1);
    assert_eq!(evaluator.stats.successors_generated, 1);
    assert_eq!(provider.active.load(Ordering::Relaxed), 0);
}
