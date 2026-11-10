use super::*;

#[tokio::test]
async fn competitive_walking_replaces_transit_with_matrix_metrics() {
    let (result, calls) = evaluate_walking_replacement(20, None, false, false).await;
    assert_eq!(calls, 1);
    assert!(result.as_ref().unwrap().journeys.iter().any(|v| {
        v.transit_pattern_sequence().is_empty()
            && v.movements().iter().any(|m| {
                matches!(
                    m.kind,
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Transfer,
                        duration_seconds: 300,
                        distance_metres: 2000
                    }
                )
            })
    }));
}

#[tokio::test]
async fn slower_walking_survives_until_transit_geometry_is_validated() {
    let (result, calls) = evaluate_walking_replacement(1, None, false, false).await;
    assert_eq!(calls, 1);
    assert_eq!(result.as_ref().unwrap().journeys.len(), 2);
    assert!(
        result.as_ref().unwrap().journeys[1]
            .transit_pattern_sequence()
            .is_empty()
    );
    assert_eq!(
        result.as_ref().unwrap().journeys[0].transit_pattern_sequence(),
        vec![TransitPatternId::new("bus")]
    );
}

#[tokio::test]
async fn optional_walking_dependency_failure_preserves_transit() {
    let (result, calls) = evaluate_walking_replacement(
        20,
        Some(crate::street::StreetRoutingError::Timeout),
        false,
        false,
    )
    .await;
    assert_eq!(calls, 1);
    assert_eq!(result.as_ref().unwrap().journeys.len(), 1);
    assert!(result.as_ref().unwrap().failures.transient);
    assert_eq!(
        result.as_ref().unwrap().journeys[0].transit_pattern_sequence(),
        vec![TransitPatternId::new("bus")]
    );
}

#[tokio::test]
async fn repeated_corridor_pairs_reuse_exact_walking_routes() {
    let (result, calls) = evaluate_walking_replacement(20, None, true, false).await;
    assert_eq!(calls, 1);
    assert!(!result.as_ref().unwrap().journeys.is_empty());
}

#[tokio::test]
async fn faster_walking_makes_downstream_transit_catchable() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["origin", "middle"]),
            pattern("second", TransitMode::Bus, &["middle", "destination"]),
        ],
        &["origin", "middle", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 1, 1);
    let retained = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 600,
            cost_source: StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("first", "origin", "middle"),
                static_leg("second", "middle", "destination"),
            ],
        }],
    );
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("first"),
        0,
        vec![trip("too-slow", "first", &["origin", "middle"], &[0, 10])],
    );
    snapshot.insert(
        TransitPatternId::new("second"),
        0,
        vec![trip(
            "catchable",
            "second",
            &["middle", "destination"],
            &[6, 12],
        )],
    );
    let street = MatrixCache::new(Arc::new(CountingWalkingRouter {
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        error: None,
    }));
    let result = evaluate_corridors(
        &network,
        &retained,
        &snapshot,
        &query,
        &street,
        None,
        &[corridor],
    )
    .await
    .unwrap();
    let caught = result
        .journeys
        .iter()
        .find(|journey| journey.transit_pattern_sequence() == vec![TransitPatternId::new("second")])
        .unwrap();
    let timing = caught.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:12 UTC));
    assert_eq!(timing.walking_seconds, 300);
    assert!(
        result
            .journeys
            .iter()
            .any(|journey| journey.transit_pattern_sequence().is_empty()
                && journey.evaluate_timing().unwrap().walking_seconds == 600)
    );
}

#[test]
fn direct_walking_corridor_has_one_unresolved_edge() {
    let origin = crate::street::Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = crate::street::Coordinate::try_new(25.02, 121.5).unwrap();
    let corridor = EvaluationCorridor::direct_walking(origin, destination);
    assert_eq!(corridor.anchors().len(), 2);
    assert_eq!(corridor.anchors()[0].kind, JourneyAnchorKind::Origin);
    assert_eq!(corridor.anchors()[1].kind, JourneyAnchorKind::Destination);
    assert_eq!(corridor.anchors()[0].coordinate, origin);
    assert_eq!(corridor.anchors()[1].coordinate, destination);
    assert!(matches!(
        corridor.edges(),
        [PlannedMovement::WalkingToResolve]
    ));
}

#[tokio::test]
async fn walking_cache_reuses_exact_requests() {
    let query = query(&[], 0, 1);
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let street = MatrixCache::new(Arc::new(CountingWalkingRouter {
        calls: calls.clone(),
        error: None,
    }));
    let from = &corridor.anchors()[0];
    let to = &corridor.anchors()[1];
    let first = walking_cell(&street, from, to).await.unwrap();
    let cached = walking_cell(&street, from, to).await.unwrap();
    assert_eq!(first, cached);
    walking_cell(&street, from, to).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!((street.requests(), street.hits()), (1, 2));
}

#[tokio::test]
async fn unreachable_walking_matrix_has_no_successor_or_provider_failure() {
    let network = network(Vec::new(), &[], Vec::new());
    let query = query(&[], 0, 1);
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let street = fake_street();
    let result = evaluate_corridors(
        &network,
        &retained,
        &snapshot,
        &query,
        &street,
        None,
        &[EvaluationCorridor::direct_walking(
            query.origin,
            query.destination,
        )],
    )
    .await
    .unwrap();
    assert!(result.journeys.is_empty());
    assert!(!result.failures.internal && !result.failures.transient);
    assert_eq!(result.stats.matrix_requests, 1);
    assert_eq!(result.stats.states_expanded, 1);
}

#[tokio::test]
async fn walking_uses_actual_matrix_timing_even_when_geometry_bound_exceeds_query_limit() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::from_retained_for_test(&network, Vec::new());
    let snapshot = TimingSnapshot::default();
    let origin = crate::street::Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = crate::street::Coordinate::try_new(25.02, 121.5).unwrap();
    let corridor = EvaluationCorridor::direct_walking(origin, destination);
    {
        for actual_seconds in [29, 31] {
            let mut query = PlanningQuery::new(
                origin,
                destination,
                crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(60)),
                Vec::new(),
                Vec::new(),
                datetime!(2026-09-21 10:00 UTC),
                crate::config::ViaConfig::default().planner_limits(),
            );
            query.limits.max_journey_duration_seconds = 30;
            let street = MatrixCache::new(Arc::new(
                crate::street::FakeStreetRouter::builder()
                    .matrix_leg(
                        crate::street::StreetTravelMode::Walking,
                        origin,
                        destination,
                        crate::street::Seconds(actual_seconds),
                        crate::street::Metres(20),
                    )
                    .build(),
            ));
            let result = evaluate_corridors(
                &network,
                &retained,
                &snapshot,
                &query,
                &street,
                None,
                std::slice::from_ref(&corridor),
            )
            .await
            .unwrap();
            assert_eq!(result.journeys.len(), usize::from(actual_seconds <= 30));
            assert_eq!(result.stats.matrix_requests, 1);
            assert_eq!(result.stats.states_expanded, 1);
        }
    }
}

#[test]
fn pure_walking_transition_preserves_metrics_and_enforces_time_bounds() {
    let mut query = query(&[], 0, 1);
    query.limits.max_journey_duration_seconds = 60;
    let state = EvaluationState::initial(&query);
    for (seconds, available_at, succeeds) in [
        (60, None, true),
        (61, None, false),
        (1, Some(query.departure_time - Duration::seconds(1)), false),
        (20, Some(query.departure_time + Duration::seconds(40)), true),
    ] {
        let next = walking_successor(
            &query,
            &state,
            Walking {
                kind: WalkingMovementKind::Access,
                duration_seconds: seconds,
                distance_metres: 123,
                access_available_at: available_at,
            },
            0,
        );
        assert_eq!(next.is_some(), succeeds);
        if let Some(next) = next {
            assert_eq!(
                next.ready_at,
                available_at
                    .unwrap_or(query.departure_time + Duration::seconds(i64::from(seconds)))
            );
            assert!(
                matches!(next.movements[0].kind, JourneyMovementKind::Walking { duration_seconds, distance_metres: 123, .. } if duration_seconds == seconds)
            );
        }
    }
    assert!(state.movements.is_empty());
    assert_eq!(state.ready_at, query.departure_time);
}

#[tokio::test]
async fn walking_operation_returns_local_failures_and_reuses_cached_results() {
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
        let street = MatrixCache::new(Arc::new(CountingWalkingRouter {
            calls: calls.clone(),
            error,
        }));
        let evaluator = CorridorEvaluator {
            network: &network,
            retained: &retained,
            timing: &snapshot,
            query: &query,
            street: &street,
            bike: None,
        };
        let run = Run::new(&evaluator);
        for _ in 0..2 {
            let successor = run
                .walking_successor(
                    &EvaluationState::initial(&query),
                    &corridor,
                    &corridor.edges()[0],
                    0,
                )
                .await
                .unwrap();
            assert_eq!(successor.is_some(), succeeds);
        }
        let failures = *run.failures.lock().unwrap();
        assert_eq!(
            (failures.transient, failures.internal),
            (transient, internal)
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(street.hits(), 1);
    }
}
