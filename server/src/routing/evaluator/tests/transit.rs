use super::*;

#[test]
fn retained_corridor_invariant_failures_are_explicit() {
    let runtime = network(Vec::new(), &["a", "b"], Vec::new());
    let make_corridor = |access: &str, egress: &str, steps| StaticCorridor {
        access_stop_id: BoardingPointId::new(access),
        egress_stop_id: BoardingPointId::new(egress),
        theoretical_duration_seconds: 0,
        cost_source: StaticCostSource::OptimisticEstimate,
        steps,
    };
    let mut query = query(&[TransitMode::Bus], 0, 1);

    let malformed = make_corridor("a", "b", Vec::new());
    assert!(matches!(
        EvaluationCorridor::from_transit(&runtime, &malformed, &query),
        Err(RoutingError::Internal(_))
    ));

    let missing_stop = make_corridor("missing", "missing", Vec::new());
    query.origin_access[0].stop_id = BoardingPointId::new("missing");
    query.destination_egress[0].stop_id = BoardingPointId::new("missing");
    assert!(matches!(
        EvaluationCorridor::from_transit(&runtime, &missing_stop, &query),
        Err(RoutingError::Internal(_))
    ));

    let no_matching_baseline = make_corridor("a", "a", Vec::new());
    assert!(matches!(
        EvaluationCorridor::from_transit(&runtime, &no_matching_baseline, &query),
        Err(RoutingError::Internal(_))
    ));
}

#[test]
fn physical_reboarding_counts_distinct_effective_trips_but_through_service_does_not() {
    let arrival = datetime!(2026-09-21 10:16 UTC);
    let boarded_twice = vec![
        realized_ride(0, trip("trip-a", "red-short", &["R10", "R09"], &[2, 4])),
        realized_ride(1, trip("trip-b", "red-full", &["R09", "R03"], &[7, 16])),
    ];
    assert_eq!(
        PathMetrics::from_movements(&boarded_twice, arrival).transfer_count,
        1
    );

    let stayed_aboard = vec![
        realized_ride(
            0,
            trip("through-trip", "red-short", &["R10", "R09"], &[2, 4]),
        ),
        realized_ride(
            1,
            trip("through-trip", "red-full", &["R09", "R03"], &[4, 16]),
        ),
    ];
    assert_eq!(
        PathMetrics::from_movements(&stayed_aboard, arrival).transfer_count,
        0
    );
}

#[test]
fn repeated_schedule_basis_does_not_prevent_direct_route_from_dominating_reboarding() {
    let arrival = datetime!(2026-09-21 10:19:22 UTC);
    let schedule_trip = |id, pattern, stops: &[&str], minutes: &[i64]| {
        let mut realized = trip(id, pattern, stops, minutes);
        realized.timing_basis = TimingBasis::Schedule;
        realized.timing_reliability = TimingReliability::Fallback;
        realized
    };
    let egress = |from_anchor| JourneyMovement {
        from_anchor,
        to_anchor: from_anchor + 1,
        departure_at: datetime!(2026-09-21 10:16 UTC),
        arrival_at: arrival,
        kind: JourneyMovementKind::Walking {
            kind: WalkingMovementKind::Egress,
            duration_seconds: 202,
            distance_metres: 100,
        },
    };
    let direct_movements = vec![
        realized_ride(
            0,
            schedule_trip("through", "red-full", &["R10", "R03"], &[2, 16]),
        ),
        egress(1),
    ];
    let split_movements = vec![
        realized_ride(
            0,
            schedule_trip("first", "red-short", &["R10", "R09"], &[2, 4]),
        ),
        realized_ride(
            1,
            schedule_trip("second", "red-full", &["R09", "R03"], &[7, 16]),
        ),
        egress(2),
    ];
    let direct = PathMetrics::from_movements(&direct_movements, arrival);
    let split = PathMetrics::from_movements(&split_movements, arrival);

    assert_eq!(direct.arrival_time, split.arrival_time);
    assert_eq!(direct.walking_seconds, split.walking_seconds);
    assert_eq!(direct.transfer_count, 0);
    assert_eq!(split.transfer_count, 1);
    assert!(direct.dominates(&split));
}

#[test]
fn phase2_single_leg_route() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip(
            "bus-trip",
            "bus",
            &["origin", "destination"],
            &[1, 20],
        )],
    );

    let (candidates, _) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:20 UTC)
    );
}

#[test]
fn phase2_multi_leg_route_keeps_explicit_surface_transfer() {
    let network = network(
        vec![
            pattern("bus", TransitMode::Bus, &["origin", "bus-end"]),
            pattern("metro", TransitMode::Metro, &["metro-start", "destination"]),
        ],
        &["origin", "bus-end", "metro-start", "destination"],
        vec![TransferEdge::new(
            BoardingPointId::new("bus-end"),
            BoardingPointId::new("metro-start"),
            30,
            100,
        )],
    );
    let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip("bus-trip", "bus", &["origin", "bus-end"], &[1, 9])],
    );
    snapshot.insert(
        TransitPatternId::new("metro"),
        0,
        vec![trip(
            "metro-trip",
            "metro",
            &["metro-start", "destination"],
            &[10, 20],
        )],
    );

    let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].evaluate_timing().unwrap().walking_seconds, 30);
    assert!(matches!(
        candidates[0]
            .movements()
            .get(2)
            .map(|movement| &movement.kind),
        Some(
            super::super::super::realized::JourneyMovementKind::Walking {
                kind: super::super::super::realized::WalkingMovementKind::Transfer,
                ..
            }
        )
    ));
}

#[test]
fn phase2_rejects_corridor_without_a_departure_after_ready_at() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 10);
    let corridor = candidate_corridor(&network, &query);
    let snapshot = TimingSnapshot::default();

    let (candidates, _) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.is_empty());
}

#[test]
fn phase2_rejects_non_monotonic_effective_trip_times() {
    let network = network(
        vec![pattern(
            "bus",
            TransitMode::Bus,
            &["origin", "middle", "destination"],
        )],
        &["origin", "middle", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip(
            "malformed-trip",
            "bus",
            &["origin", "middle", "destination"],
            &[1, 5, 4],
        )],
    );

    let (candidates, _) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.is_empty());
}

#[test]
fn phase2_later_departure_can_have_earlier_arrival() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early-slow", "bus", &["origin", "destination"], &[1, 30]),
            trip("late-fast", "bus", &["origin", "destination"], &[4, 15]),
        ],
    );

    let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[1].evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:30 UTC)
    );
    assert_eq!(
        candidates[0].evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:15 UTC)
    );
    assert_eq!(
        candidates[0].transit_pattern_sequence(),
        vec![TransitPatternId::new("bus")]
    );
    assert!(matches!(
        candidates[0]
            .movements()
            .get(1)
            .map(|movement| &movement.kind),
        Some(super::super::super::realized::JourneyMovementKind::Transit { .. })
    ));
}

#[test]
fn phase2_respects_maximum_journey_duration_and_latest_arrival() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let corridor_query = query(&[TransitMode::Bus], 0, 10);
    let corridor = candidate_corridor(&network, &corridor_query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip(
            "late-trip",
            "bus",
            &["origin", "destination"],
            &[1, 20],
        )],
    );

    let short_duration_query = corridor_query
        .clone()
        .with_limits(crate::routing::PlannerLimits {
            max_transfers: 0,
            max_candidates: 16,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 10 * 60,
            ..crate::routing::PlannerLimits::default()
        });
    assert!(
        evaluate(&network, &corridor, &snapshot, short_duration_query)
            .unwrap()
            .is_empty()
    );

    let mut latest_arrival_query = corridor_query;
    latest_arrival_query.latest_arrival = Some(datetime!(2026-09-21 10:10 UTC));
    assert!(
        evaluate(&network, &corridor, &snapshot, latest_arrival_query)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn phase2_applies_same_stop_transfer_time() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["origin", "middle"]),
            pattern("second", TransitMode::Metro, &["middle", "destination"]),
        ],
        &["origin", "middle", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("first"),
        0,
        vec![trip("first-trip", "first", &["origin", "middle"], &[1, 10])],
    );
    snapshot.insert(
        TransitPatternId::new("second"),
        0,
        vec![trip(
            "too-soon",
            "second",
            &["middle", "destination"],
            &[11, 20],
        )],
    );

    let (candidates, stats) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.is_empty());
    assert!(network.same_stop_transfer_seconds() >= 60);
    assert!(stats.transit_departure_lookups > 0);
}

#[test]
fn explicit_surface_transfer_does_not_add_same_stop_penalty() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["origin", "bus-end"]),
            pattern(
                "second",
                TransitMode::Metro,
                &["metro-start", "destination"],
            ),
        ],
        &["origin", "bus-end", "metro-start", "destination"],
        vec![TransferEdge::new(
            BoardingPointId::new("bus-end"),
            BoardingPointId::new("metro-start"),
            30,
            100,
        )],
    );
    let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("first"),
        0,
        vec![trip("first-trip", "first", &["origin", "bus-end"], &[1, 9])],
    );
    snapshot.insert(
        TransitPatternId::new("second"),
        0,
        vec![trip(
            "second-trip",
            "second",
            &["metro-start", "destination"],
            &[10, 20],
        )],
    );

    let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:20 UTC)
    );
}

#[test]
fn phase2_allows_equivalent_pattern_substitution() {
    let network = network(
        vec![
            pattern("pattern-a", TransitMode::Bus, &["origin", "destination"]),
            pattern("pattern-b", TransitMode::Bus, &["origin", "destination"]),
        ],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let corridor = candidate_corridor(&network, &query);
    let retained_leg = corridor.retained()[0].transit_legs().next().unwrap();
    let equivalent_legs = corridor.equivalent_legs_for(&network, retained_leg);
    assert_eq!(equivalent_legs.len(), 2);
    let equivalent_leg = equivalent_legs
        .iter()
        .find(|leg| leg.pattern_id != retained_leg.pattern_id)
        .unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        equivalent_leg.pattern_id.clone(),
        equivalent_leg.board_index,
        vec![trip(
            "equivalent-trip",
            equivalent_leg.pattern_id.as_str(),
            &["origin", "destination"],
            &[1, 10],
        )],
    );

    let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].transit_pattern_sequence(),
        vec![equivalent_leg.pattern_id.clone()]
    );
}

#[test]
fn phase2_never_combines_different_retained_corridors() {
    let network = network(
        vec![
            pattern("a-bus-prefix", TransitMode::Bus, &["origin", "middle"]),
            pattern("b-bus-suffix", TransitMode::Bus, &["middle", "destination"]),
            pattern("c-metro-prefix", TransitMode::Metro, &["origin", "middle"]),
            pattern(
                "d-metro-suffix",
                TransitMode::Metro,
                &["middle", "destination"],
            ),
        ],
        &["origin", "middle", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 2);
    let retained = vec![
        StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 1,
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("a-bus-prefix", "origin", "middle"),
                static_leg("b-bus-suffix", "middle", "destination"),
            ],
        },
        StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 2,
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("c-metro-prefix", "origin", "middle"),
                static_leg("d-metro-suffix", "middle", "destination"),
            ],
        },
    ];
    let corridor = RetainedCorridorSet::from_retained_for_test(&network, retained);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("a-bus-prefix"),
        0,
        vec![trip(
            "a-prefix-trip",
            "a-bus-prefix",
            &["origin", "middle"],
            &[1, 5],
        )],
    );
    snapshot.insert(
        TransitPatternId::new("b-bus-suffix"),
        0,
        vec![trip(
            "b-suffix-trip",
            "b-bus-suffix",
            &["middle", "destination"],
            &[16, 30],
        )],
    );
    snapshot.insert(
        TransitPatternId::new("c-metro-prefix"),
        0,
        vec![trip(
            "z-prefix-trip",
            "c-metro-prefix",
            &["origin", "middle"],
            &[1, 5],
        )],
    );
    snapshot.insert(
        TransitPatternId::new("d-metro-suffix"),
        0,
        vec![trip(
            "d-suffix-trip",
            "d-metro-suffix",
            &["middle", "destination"],
            &[10, 18],
        )],
    );

    let (candidates, _) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.iter().all(|candidate| {
        let sequence = candidate.transit_pattern_sequence();
        sequence
            == [
                TransitPatternId::new("a-bus-prefix"),
                TransitPatternId::new("b-bus-suffix"),
            ]
            || sequence
                == [
                    TransitPatternId::new("c-metro-prefix"),
                    TransitPatternId::new("d-metro-suffix"),
                ]
    }));
    assert!(!candidates.is_empty());
}

#[test]
fn transit_corridor_preserves_planned_movement_and_anchor_order() {
    let network = network(
        vec![
            pattern("bus", TransitMode::Bus, &["origin", "a"]),
            pattern("metro", TransitMode::Metro, &["b", "destination"]),
        ],
        &["origin", "a", "b", "destination"],
        vec![TransferEdge::new(
            BoardingPointId::new("a"),
            BoardingPointId::new("b"),
            60,
            100,
        )],
    );
    let mut query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
    query.origin_access.push(query.origin_access[0].clone());
    query
        .destination_egress
        .push(query.destination_egress[0].clone());
    let retained = candidate_corridor(&network, &query);
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let expected = [
        JourneyAnchorKind::Origin,
        JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new("origin"),
        },
        JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new("a"),
        },
        JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new("b"),
        },
        JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new("destination"),
        },
        JourneyAnchorKind::Destination,
    ];
    assert_eq!(
        corridor
            .anchors()
            .iter()
            .map(|a| &a.kind)
            .collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    assert_eq!(corridor.edges().len(), corridor.anchors().len() - 1);
    assert!(matches!(
        corridor.edges()[0].planned_movement,
        PlannedMovement::RetainedWalking {
            kind: WalkingMovementKind::Access,
            duration_seconds: 0,
            distance_metres: 0,
            access_available_at: Some(_)
        }
    ));
    assert!(
        matches!(&corridor.edges()[1].planned_movement, PlannedMovement::Transit(leg) if leg.pattern_id == TransitPatternId::new("bus"))
    );
    assert!(matches!(
        corridor.edges()[2].planned_movement,
        PlannedMovement::RetainedWalking {
            kind: WalkingMovementKind::Transfer,
            duration_seconds: 60,
            distance_metres: 100,
            access_available_at: None
        }
    ));
    assert!(
        matches!(&corridor.edges()[3].planned_movement, PlannedMovement::Transit(leg) if leg.pattern_id == TransitPatternId::new("metro"))
    );
    assert!(matches!(
        corridor.edges()[4].planned_movement,
        PlannedMovement::RetainedWalking {
            kind: WalkingMovementKind::Egress,
            duration_seconds: 0,
            distance_metres: 0,
            access_available_at: None
        }
    ));
}

#[test]
fn pure_transit_transition_validates_arrivals_and_transfer_readiness() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let leg = retained.retained()[0].transit_legs().next().unwrap();
    let state = EvaluationState::initial(&query);
    let ride = trip("ride", "bus", &["origin", "destination"], &[1, 5]);
    let next =
        expansion::transit_next_state(&network, &query, &state, &ride, leg, 1, state.ready_at)
            .unwrap();
    assert_eq!(next.ready_at, query.departure_time + Duration::minutes(5));
    assert!(next.last_was_transit);
    assert_eq!(
        expansion::transit_ready_at(&next, 120),
        next.ready_at + Duration::seconds(120)
    );
    assert!(
        expansion::transit_next_state(&network, &query, &next, &ride, leg, 1, next.ready_at)
            .is_none()
    );
    let invalid = trip("invalid", "bus", &["origin", "destination"], &[5, 1]);
    assert!(
        expansion::transit_next_state(&network, &query, &state, &invalid, leg, 1, state.ready_at)
            .is_none()
    );
    let mut limited = query.clone();
    limited.limits.max_journey_duration_seconds = 299;
    assert!(
        expansion::transit_next_state(&network, &limited, &state, &ride, leg, 1, state.ready_at)
            .is_none()
    );
    assert!(state.movements.is_empty());
}
