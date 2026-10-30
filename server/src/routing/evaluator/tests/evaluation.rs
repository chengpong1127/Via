use super::*;

#[test]
fn long_corridor_only_checks_its_next_edges_before_exact_routing() {
    let stop_names = (0..=12)
        .map(|index| format!("stop-{index}"))
        .collect::<Vec<_>>();
    let stop_refs = stop_names.iter().map(String::as_str).collect::<Vec<_>>();
    let network = network(Vec::new(), &stop_refs, Vec::new());
    let departure_at = datetime!(2026-09-21 10:00 UTC);
    let query = PlanningQuery::new(
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
        vec![AccessPoint::new(
            BoardingPointId::new(&stop_names[0]),
            departure_at,
            departure_at,
            0,
            0,
        )],
        vec![EgressPoint::new(
            BoardingPointId::new(stop_names.last().unwrap()),
            0,
            0,
        )],
        departure_at,
        crate::config::ViaConfig::default().planner_limits(),
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new(&stop_names[0]),
            egress_stop_id: BoardingPointId::new(stop_names.last().unwrap()),
            theoretical_duration_seconds: 12 * 60,
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
            steps: stop_names
                .windows(2)
                .map(|pair| StaticCorridorStep::SurfaceTransfer {
                    from: BoardingPointId::new(&pair[0]),
                    to: BoardingPointId::new(&pair[1]),
                    walking_duration_seconds: 60,
                    walking_distance_metres: 100,
                })
                .collect(),
        }],
    );
    let realizations = (0..stop_names.len() - 1)
        .flat_map(|from| {
            let stop_names = &stop_names;
            (from + 1..stop_names.len()).map(move |to| {
                (
                    corridor_movement(from, to, &stop_names[from], &stop_names[to]),
                    vec![bike_movement(from, to, 20 * 60)],
                )
            })
        })
        .collect::<HashMap<_, _>>();

    let (_, stats) = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap();

    eprintln!("long corridor stats: {stats:?}");
    assert_eq!(stats.bike_exact_candidates_requested, 57);
    assert_eq!(stats.bike_exact_cache_hits, 86);
}

#[test]
fn expired_deadline_stops_phase2_before_evaluating_corridors() {
    let network = network(Vec::new(), &["origin", "destination"], Vec::new());
    let corridor = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[TransitMode::Bus], 0, 1);
    query.deadline = super::super::super::model::PlanningDeadline::elapsed_for_test();

    let result = evaluate_with_stats(&network, &corridor, &snapshot, query);

    assert_eq!(result.unwrap_err(), RoutingError::DeadlineExceeded);
}

#[tokio::test]
async fn transit_expansion_emits_multiple_departures_before_pruning() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early", "bus", &["origin", "destination"], &[1, 10]),
            trip("later", "bus", &["origin", "destination"], &[2, 12]),
        ],
    );
    let planners = test_dependencies();
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &planners.walking,
        bike_share: planners.bike_share.as_ref(),
    };
    let outcome = resolver
        .expand_state(
            &EvaluationState::initial(&query),
            &corridor,
            &corridor.edges()[1],
            1,
        )
        .await;
    let next = outcome.successors.unwrap();
    assert_eq!(next.len(), 2);
    let trip_ids = next
        .iter()
        .map(|state| match &state.movements[0].kind {
            JourneyMovementKind::Transit { trip, .. } => trip.id.clone(),
            _ => panic!("expected transit"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        trip_ids,
        vec![EffectiveTripId::new("early"), EffectiveTripId::new("later")]
    );
    assert_eq!(outcome.stats.transit_departure_lookups, 1);
}

#[tokio::test]
async fn expansion_preserves_transit_walking_and_bike_successor_order() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early", "bus", &["origin", "destination"], &[1, 2]),
            trip("later", "bus", &["origin", "destination"], &[2, 3]),
        ],
    );
    let planners = TestDependencies {
        walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: None,
        })),
        bike_share: Some(BikeShareSession::for_test(Box::new(TestBikePlanner {
            routes: HashMap::from([(
                corridor_movement(0, 1, "origin", "destination"),
                vec![bike_movement(0, 1, 240)],
            )]),
            cache: Default::default(),
            hits: Default::default(),
        }))),
    };
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &planners.walking,
        bike_share: planners.bike_share.as_ref(),
    };
    let outcome = resolver
        .expand_state(
            &EvaluationState::initial(&query),
            &corridor,
            &corridor.edges()[1],
            1,
        )
        .await;
    let next = outcome.successors.unwrap();
    let kinds = next
        .iter()
        .map(|state| match &state.movements[0].kind {
            JourneyMovementKind::Transit { trip, .. } => trip.id.as_str(),
            JourneyMovementKind::Walking { .. } => "walking",
            JourneyMovementKind::BikeShare { .. } => "bike_share",
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec!["early", "later", "walking", "bike_share"]);
    assert!(next[2].ready_at > next[0].ready_at);
    assert!(next[3].ready_at > next[0].ready_at);
    assert_eq!(outcome.stats.states_expanded, 1);
    assert_eq!(outcome.stats.transit_operations_attempted, 1);
    assert_eq!(outcome.stats.walking_operations_attempted, 1);
    assert_eq!(outcome.stats.bike_operations_attempted, 1);
    assert_eq!(outcome.stats.successors_generated, 4);
    assert_eq!(outcome.stats.transit_departure_lookups, 1);
}

#[tokio::test]
async fn surface_realization_completes_alongside_transit() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip("ride", "bus", &["origin", "destination"], &[0, 10])],
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let planners = TestDependencies {
        walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: calls.clone(),
            error: None,
        })),
        bike_share: None,
    };
    let evaluator = CorridorScheduler::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    );
    let report = evaluator.evaluate(&[corridor]).await;
    assert!(!report.outcome.unwrap().is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(report.stats.corridors_completed, 1);
}

#[tokio::test]
async fn direct_corridor_has_observable_constant_work() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 1);
    query.destination = crate::street::Coordinate::try_new(25.02, 121.5).unwrap();
    let retained = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip("ride", "bus", &["origin", "destination"], &[1, 10])],
    );
    let mut measurements = Vec::new();
    for include_direct in [false, true] {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let planners = TestDependencies {
            walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
                calls: calls.clone(),
                error: None,
            })),
            bike_share: None,
        };
        let mut corridors = vec![
            EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap(),
        ];
        if include_direct {
            corridors.insert(
                0,
                EvaluationCorridor::direct_walking(query.origin, query.destination),
            );
        }
        let result = CorridorScheduler::new(
            &network,
            &retained,
            &snapshot,
            &query,
            &planners.walking,
            planners.bike_share.as_ref(),
        )
        .evaluate(&corridors)
        .await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            if include_direct { 2 } else { 1 }
        );
        if include_direct {
            assert!(
                result
                    .outcome
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|j| j.anchors().len() == 2)
            );
            assert!(
                result
                    .outcome
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|j| !j.transit_pattern_sequence().is_empty())
            );
        }
        measurements.push(result.stats);
    }
    assert_eq!(
        measurements[1].states_expanded,
        measurements[0].states_expanded + 1
    );
    assert_eq!(
        measurements[1].transit_departure_lookups,
        measurements[0].transit_departure_lookups
    );
}

#[tokio::test(start_paused = true)]
async fn deadline_remains_a_hard_failure_after_a_completed_corridor() {
    let network = network(Vec::new(), &[], Vec::new());
    let mut query = query(&[], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let corridors = [
        EvaluationCorridor::direct_walking(query.origin, query.destination),
        EvaluationCorridor::direct_walking(
            query.origin,
            crate::street::Coordinate::try_new(25.001, 121.5).unwrap(),
        ),
    ];
    let planners = TestDependencies {
        walking: WalkingCache::new(Arc::new(SlowSecondWalkingRouter {
            calls: std::sync::atomic::AtomicUsize::new(0),
        })),
        bike_share: None,
    };
    let evaluator = CorridorScheduler::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    );
    let report = evaluator.evaluate(&corridors).await;
    assert!(matches!(
        report.outcome,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(report.stats.corridors_completed, 1);
}

#[tokio::test]
async fn direct_corridor_returns_both_resolved_surface_successors() {
    let network = network(Vec::new(), &[], Vec::new());
    let query = query(&[], 0, 1);
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let planners = TestDependencies {
        walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: None,
        })),
        bike_share: Some(BikeShareSession::for_test(Box::new(DirectBikePlanner {
            movement: bike_movement(0, 1, 240),
        }))),
    };
    let evaluator = CorridorScheduler::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    );
    let report = evaluator.evaluate(&[corridor]).await;
    assert_eq!(report.outcome.as_ref().unwrap().len(), 2);
    assert_eq!(report.stats.states_expanded, 1);
    assert_eq!(report.stats.successors_generated, 2);
    assert_eq!(report.stats.walking_operations_attempted, 1);
    assert_eq!(report.stats.bike_operations_attempted, 1);
    assert_eq!(report.stats.corridors_completed, 1);
    assert_eq!(report.stats.walking_matrix_requests, 1);
}

#[tokio::test]
async fn every_prepared_corridor_completes_beyond_the_former_work_boundary() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::disabled();
    let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        error: None,
    }));
    let corridors = (0..2400)
        .map(|_| EvaluationCorridor::direct_walking(query.origin, query.destination))
        .collect::<Vec<_>>();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        CorridorScheduler::new(&network, &retained, &snapshot, &query, &walking, None)
            .evaluate(&corridors),
    )
    .await
    .unwrap();
    assert_eq!(result.stats.corridors_attempted, 2400);
    assert_eq!(result.stats.corridors_completed, 2400);
    assert_eq!(result.stats.states_expanded, 2400);
    assert_eq!(result.stats.walking_operations_attempted, 2400);
    assert_eq!(result.stats.successors_generated, 2400);
    assert_eq!(result.stats.walking_matrix_requests, 1);
    assert_eq!(result.stats.walking_cache_hits, 2399);
    assert_eq!(result.outcome.as_ref().unwrap().len(), 1);
}

#[tokio::test]
async fn operation_outcomes_sum_to_the_state_outcome_without_extra_matrix_requests() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early", "bus", &["origin", "destination"], &[1, 2]),
            trip("later", "bus", &["origin", "destination"], &[2, 3]),
        ],
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
        calls: calls.clone(),
        error: None,
    }));
    let bike = BikeShareSession::for_test(Box::new(TestBikePlanner {
        routes: HashMap::from([(
            corridor_movement(0, 1, "origin", "destination"),
            vec![bike_movement(0, 1, 60), bike_movement(0, 1, 120)],
        )]),
        cache: Default::default(),
        hits: Default::default(),
    }));
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &walking,
        bike_share: Some(&bike),
    };
    let state = EvaluationState::initial(&query);
    let transit = resolver
        .inputs
        .resolve_transit(&state, &corridor.edges()[1], 1);
    assert_eq!(
        transit.stats,
        expansion::ExpansionStats {
            transit_operations_attempted: 1,
            transit_departure_lookups: 1,
            successors_generated: 2,
            ..Default::default()
        }
    );
    let walking = resolver
        .resolve_walking(&state, &corridor, &corridor.edges()[1], 1)
        .await;
    assert_eq!(
        walking.stats,
        expansion::ExpansionStats {
            walking_operations_attempted: 1,
            successors_generated: 1,
            ..Default::default()
        }
    );
    let bike = resolver.resolve_bike_share(&state, &corridor, 1).await;
    assert_eq!(
        bike.stats,
        expansion::ExpansionStats {
            bike_operations_attempted: 1,
            successors_generated: 2,
            ..Default::default()
        }
    );
    let mut expected_stats = expansion::ExpansionStats {
        states_expanded: 1,
        ..Default::default()
    };
    let mut expected_movements = Vec::new();
    for operation in [transit, walking, bike] {
        expected_stats.merge(operation.stats);
        assert!(!operation.failures.transient && !operation.failures.internal);
        expected_movements.extend(
            operation
                .successors
                .unwrap()
                .into_iter()
                .map(|state| state.movements),
        );
    }
    let outcome = resolver
        .expand_state(&state, &corridor, &corridor.edges()[1], 1)
        .await;
    assert_eq!(outcome.stats, expected_stats);
    assert_eq!(
        outcome
            .successors
            .unwrap()
            .into_iter()
            .map(|state| state.movements)
            .collect::<Vec<_>>(),
        expected_movements
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(resolver.walking.cache_hits(), 1);
    assert_eq!(
        resolver
            .bike_share
            .as_ref()
            .unwrap()
            .realization_stats()
            .exact_requests,
        1
    );
    assert_eq!(
        resolver
            .bike_share
            .as_ref()
            .unwrap()
            .realization_stats()
            .cache_hits,
        1
    );
}

#[tokio::test]
async fn bike_operation_returns_local_failures_and_keeps_walking_fallback() {
    use crate::street::StreetRoutingError;
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let query = query(&[], 0, 1);
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    for (error, transient, internal) in [
        (None, false, false),
        (Some(StreetRoutingError::Timeout), true, false),
        (Some(StreetRoutingError::MalformedResponse), false, true),
        (Some(StreetRoutingError::Unreachable), false, false),
    ] {
        let succeeds = error.is_none();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: None,
        }));
        let bike = BikeShareSession::for_test(Box::new(OutcomeBikeProvider {
            calls: calls.clone(),
            error,
            expire_deadline: false,
        }));
        let resolver = expansion::ExpansionResolver {
            inputs: expansion_inputs(&network, &retained, &snapshot, &query),
            walking: &walking,
            bike_share: Some(&bike),
        };
        let state = EvaluationState::initial(&query);
        let operation = resolver.resolve_bike_share(&state, &corridor, 0).await;
        assert_eq!(
            operation.stats,
            expansion::ExpansionStats {
                bike_operations_attempted: 1,
                successors_generated: usize::from(succeeds),
                ..Default::default()
            }
        );
        assert_eq!(
            (operation.failures.transient, operation.failures.internal),
            (transient, internal)
        );
        let outcome = resolver
            .expand_state(&state, &corridor, &corridor.edges()[0], 0)
            .await;
        assert_eq!(
            (outcome.failures.transient, outcome.failures.internal),
            (transient, internal)
        );
        assert_eq!(outcome.successors.unwrap().len(), 1 + usize::from(succeeds));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(resolver.walking.requests(), 1);
    }
}

#[tokio::test]
async fn fatal_transit_outcome_preserves_counts_and_skips_both_surface_operations() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let snapshot = TimingSnapshot::default();
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut leg = retained.retained()[0]
        .transit_legs()
        .next()
        .unwrap()
        .clone();
    leg.alight_index = leg.board_index;
    let edge = CorridorEdge {
        planned_movement: PlannedMovement::Transit(leg),
    };
    let walking_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bike_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
        calls: walking_calls.clone(),
        error: None,
    }));
    let bike = BikeShareSession::for_test(Box::new(OutcomeBikeProvider {
        calls: bike_calls.clone(),
        error: None,
        expire_deadline: false,
    }));
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &walking,
        bike_share: Some(&bike),
    };
    let outcome = resolver
        .expand_state(&EvaluationState::initial(&query), &corridor, &edge, 1)
        .await;
    assert!(matches!(outcome.successors, Err(RoutingError::Internal(_))));
    assert_eq!(
        outcome.stats,
        expansion::ExpansionStats {
            states_expanded: 1,
            transit_operations_attempted: 1,
            ..Default::default()
        }
    );
    assert_eq!(walking_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(bike_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn fatal_walking_outcome_preserves_transit_successor_counts_and_skips_bike() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    let retained = candidate_corridor(&network, &query);
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early", "bus", &["origin", "destination"], &[1, 2]),
            trip("later", "bus", &["origin", "destination"], &[2, 3]),
        ],
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let walking = WalkingCache::new(Arc::new(DeadlineWalkingRouter));
    let bike = BikeShareSession::for_test(Box::new(OutcomeBikeProvider {
        calls: calls.clone(),
        error: None,
        expire_deadline: false,
    }));
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &walking,
        bike_share: Some(&bike),
    };
    let outcome = resolver
        .expand_state(
            &EvaluationState::initial(&query),
            &corridor,
            &corridor.edges()[1],
            1,
        )
        .await;
    assert!(matches!(
        outcome.successors,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(
        outcome.stats,
        expansion::ExpansionStats {
            states_expanded: 1,
            transit_operations_attempted: 1,
            transit_departure_lookups: 1,
            walking_operations_attempted: 1,
            successors_generated: 2,
            ..Default::default()
        }
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn fatal_bike_outcome_keeps_the_earlier_walking_failure() {
    let network = network(Vec::new(), &[], Vec::new());
    let mut query = query(&[], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        error: Some(crate::street::StreetRoutingError::Timeout),
    }));
    let bike = BikeShareSession::for_test(Box::new(OutcomeBikeProvider {
        calls: calls.clone(),
        error: None,
        expire_deadline: true,
    }));
    let resolver = expansion::ExpansionResolver {
        inputs: expansion_inputs(&network, &retained, &snapshot, &query),
        walking: &walking,
        bike_share: Some(&bike),
    };
    let outcome = resolver
        .expand_state(
            &EvaluationState::initial(&query),
            &corridor,
            &corridor.edges()[0],
            0,
        )
        .await;
    assert!(matches!(
        outcome.successors,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(
        outcome.stats,
        expansion::ExpansionStats {
            states_expanded: 1,
            walking_operations_attempted: 1,
            bike_operations_attempted: 1,
            ..Default::default()
        }
    );
    assert!(outcome.failures.transient && !outcome.failures.internal);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn evaluator_aggregates_outcomes_before_propagating_a_fatal_walking_error() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(1));
    let retained = candidate_corridor(&network, &query);
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early", "bus", &["origin", "destination"], &[1, 2]),
            trip("later", "bus", &["origin", "destination"], &[2, 3]),
        ],
    );
    let walking = WalkingCache::new(Arc::new(DeadlineWalkingRouter));
    let evaluator = CorridorScheduler::new(&network, &retained, &snapshot, &query, &walking, None);
    let report = evaluator.evaluate(&[corridor]).await;
    assert!(matches!(
        report.outcome,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(report.stats.states_expanded, 2);
    assert_eq!(report.stats.walking_operations_attempted, 2);
    assert_eq!(report.stats.transit_operations_attempted, 1);
    assert_eq!(report.stats.transit_departure_lookups, 1);
    assert_eq!(report.stats.successors_generated, 3);
    assert_eq!(report.stats.corridors_completed, 0);
}
