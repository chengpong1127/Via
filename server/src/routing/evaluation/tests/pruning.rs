use super::*;

#[test]
fn unhydrated_walking_does_not_discard_a_distinct_bike_path() {
    let network = network(Vec::new(), &["a", "b"], Vec::new());
    let query = PlanningQuery::new(
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
        crate::config::ViaConfig::default().planner_limits(),
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 60,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 60,
                walking_distance_metres: 100,
            }],
        }],
    );
    let bike = BikeShareMovement::new(
        0,
        1,
        BikeStationId::new("pickup"),
        BikeStationId::new("dropoff"),
        BikeWalkingAccess::new(30, 15),
        crate::routing::BikeRide {
            duration_seconds: 30,
            distance_metres: 100,
        },
        BikeWalkingAccess::new(30, 15),
    )
    .unwrap();
    let realizations = HashMap::from([(corridor_movement(0, 1, "a", "b"), vec![bike])]);

    let realized_journeys = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap()
    .0;

    assert_eq!(realized_journeys.len(), 2);
    assert!(
        realized_journeys
            .iter()
            .any(|journey| journey.has_bike_share())
    );
    assert!(
        realized_journeys
            .iter()
            .any(|journey| !journey.has_bike_share())
    );
}

#[test]
fn pareto_keeps_a_later_route_with_less_walking() {
    let network = network(
        vec![
            pattern("walky", TransitMode::Bus, &["a", "destination"]),
            pattern("lowwalk", TransitMode::Bus, &["x", "destination"]),
        ],
        &["a", "x", "destination"],
        Vec::new(),
    );
    let mut query = PlanningQuery::new(
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
        vec![
            AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:02 UTC),
                120,
                600,
            ),
            AccessPoint::new(
                BoardingPointId::new("x"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            ),
        ],
        vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
        crate::config::ViaConfig::default().planner_limits(),
    );
    query.limits.max_candidates = 10;
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![
            StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 300,
                cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
                steps: vec![static_leg("walky", "a", "destination")],
            },
            StaticCorridor {
                access_stop_id: BoardingPointId::new("x"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 480,
                cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
                steps: vec![static_leg("lowwalk", "x", "destination")],
            },
        ],
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("walky"),
        0,
        vec![trip("walky-trip", "walky", &["a", "destination"], &[2, 5])],
    );
    snapshot.insert(
        TransitPatternId::new("lowwalk"),
        0,
        vec![trip(
            "lowwalk-trip",
            "lowwalk",
            &["x", "destination"],
            &[4, 8],
        )],
    );

    let realized_journeys = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &snapshot,
        query,
        &TestBikeRoutes::new(),
    )
    .unwrap()
    .0;

    assert_eq!(realized_journeys.len(), 2);
    let metrics = realized_journeys
        .iter()
        .map(|journey| journey.evaluate_timing().unwrap())
        .collect::<Vec<_>>();
    assert!(
        metrics
            .iter()
            .any(|item| item.arrival_at == datetime!(2026-09-21 10:05 UTC)
                && item.walking_seconds == 120)
    );
    assert!(metrics.iter().any(
        |item| item.arrival_at == datetime!(2026-09-21 10:08 UTC) && item.walking_seconds == 0
    ));
}

#[test]
fn phase2_keeps_reliable_candidate_when_fallback_arrives_earlier() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut fallback = trip("a-fallback", "bus", &["origin", "destination"], &[1, 5]);
    fallback.timing_reliability = TimingReliability::Fallback;
    let reliable = trip("z-reliable", "bus", &["origin", "destination"], &[2, 6]);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(TransitPatternId::new("bus"), 0, vec![fallback, reliable]);

    let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.iter().any(|candidate| {
        candidate.movements().iter().any(|movement| {
            matches!(
                &movement.kind,
                super::super::super::journey::JourneyMovementKind::Transit { trip, .. }
                    if trip.timing_reliability == TimingReliability::Reliable
            )
        })
    }));
}

#[test]
fn phase2_state_pruning_keeps_reliability_tradeoffs_for_later_legs() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["origin", "transfer"]),
            pattern("second", TransitMode::Bus, &["transfer", "destination"]),
        ],
        &["origin", "transfer", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 1, 10);
    let corridor = candidate_corridor(&network, &query);
    let mut fallback = trip("a-fallback", "first", &["origin", "transfer"], &[1, 3]);
    fallback.timing_reliability = TimingReliability::Fallback;
    let reliable = trip("z-reliable", "first", &["origin", "transfer"], &[1, 3]);
    let second = trip(
        "second-trip",
        "second",
        &["transfer", "destination"],
        &[5, 10],
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(TransitPatternId::new("first"), 0, vec![fallback, reliable]);
    snapshot.insert(TransitPatternId::new("second"), 0, vec![second]);

    let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.iter().any(|candidate| {
        candidate.movements().iter().any(|movement| {
            matches!(
                &movement.kind,
                super::super::super::journey::JourneyMovementKind::Transit { trip, .. }
                    if trip.id == EffectiveTripId::new("z-reliable")
            )
        })
    }));
}

#[test]
fn phase2_caps_each_edge_frontier_after_pareto_pruning() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 10);
    query.limits.max_candidates = 2;
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    let mut departures = vec![
        trip("early-arrival", "bus", &["origin", "destination"], &[1, 5]),
        trip("middle-arrival", "bus", &["origin", "destination"], &[4, 8]),
        trip("late-arrival", "bus", &["origin", "destination"], &[6, 10]),
    ];
    departures[1].timing_basis = TimingBasis::Schedule;
    departures[2].timing_reliability = TimingReliability::Fallback;
    snapshot.insert(TransitPatternId::new("bus"), 0, departures);
    let (candidates, stats) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(stats.final_candidates, 2);
    assert_eq!(candidates.len(), 2);
}

#[test]
fn state_pruning_preserves_rental_and_transit_capabilities_across_modes() {
    let query = query(&[], 0, 1);
    let initial = EvaluationState::initial(&query);
    let walk = initial.after_movement(JourneyMovement {
        from_anchor: 0,
        to_anchor: 1,
        departure_at: query.departure_time,
        arrival_at: query.departure_time,
        kind: JourneyMovementKind::Walking {
            kind: WalkingMovementKind::Transfer,
            duration_seconds: 0,
            distance_metres: 0,
        },
    });
    let transit = initial.after_movement(realized_ride(
        0,
        trip("ride", "bus", &["origin", "destination"], &[0, 0]),
    ));
    let bike = initial
        .after_bike_share(
            bike_movement(0, 1, 60),
            0,
            1,
            query.departure_time,
            query.departure_time + Duration::seconds(60),
            query.departure_time,
        )
        .unwrap();
    let frontier = prune_states(vec![walk.clone(), walk, transit, bike], 16);
    assert_eq!(frontier.len(), 3);
    assert!(frontier.iter().any(|state| state.last_was_transit));
    assert!(
        frontier
            .iter()
            .any(|state| state.active_bike_rental.is_some())
    );
    assert!(
        frontier
            .iter()
            .any(|state| !state.last_was_transit && state.active_bike_rental.is_none())
    );
}

#[test]
fn final_pareto_matches_original_for_ties_evidence_and_surface_paths() {
    use crate::domain::realtime::{
        ArrivalEstimate, ArrivalState, FreshnessState, NormalizedRealtimeValue,
    };

    let start = datetime!(2026-09-21 10:00 UTC);
    let make = |movement: JourneyMovement| {
        let metrics =
            PathMetrics::from_movements(std::slice::from_ref(&movement), movement.arrival_at);
        let journey = RealizedJourney::from_movements(
            vec![
                JourneyAnchor {
                    kind: JourneyAnchorKind::Origin,
                    reached_at: start,
                },
                JourneyAnchor {
                    kind: JourneyAnchorKind::Destination,
                    reached_at: movement.arrival_at,
                },
            ],
            vec![movement],
            start,
        )
        .unwrap();
        EvaluatedCandidate { metrics, journey }
    };
    let transit = make(realized_ride(
        0,
        trip("a", "bus", &["origin", "destination"], &[1, 5]),
    ));
    let mut tied = make(realized_ride(
        0,
        trip("b", "bus", &["origin", "destination"], &[1, 5]),
    ));
    let mut later = make(realized_ride(
        0,
        trip("c", "bus", &["origin", "destination"], &[1, 7]),
    ));
    let mut fallback = make(realized_ride(
        0,
        trip("d", "bus", &["origin", "destination"], &[1, 8]),
    ));
    fallback.metrics.timing_reliability = TimingReliability::Fallback;
    tied.metrics.evidence.push(NormalizedRealtimeValue {
        freshness: FreshnessState::Fresh,
        value: ArrivalEstimate {
            transit_pattern_id: TransitPatternId::new("bus"),
            boarding_point_id: BoardingPointId::new("origin"),
            predicted_arrival_at: None,
            state: ArrivalState::Unknown,
            transmitted_at: start,
        },
    });
    later.metrics.walking_seconds = 30;
    let walk = make(JourneyMovement {
        from_anchor: 0,
        to_anchor: 1,
        departure_at: start,
        arrival_at: start + Duration::minutes(6),
        kind: JourneyMovementKind::Walking {
            kind: WalkingMovementKind::Direct,
            duration_seconds: 360,
            distance_metres: 300,
        },
    });
    let bike = make(JourneyMovement {
        from_anchor: 0,
        to_anchor: 1,
        departure_at: start,
        arrival_at: start + Duration::minutes(4),
        kind: JourneyMovementKind::BikeShare {
            movement: bike_movement(0, 1, 240),
        },
    });
    let pool = vec![later, transit.clone(), tied, fallback, walk, bike, transit];
    for rotation in 0..pool.len() {
        let mut input = pool.clone();
        input.rotate_left(rotation);
        let expected = pareto_reference::reference(input.clone());
        let actual = pareto_candidates(input);
        pareto_reference::assert_equal(&expected, &actual);
    }
}

#[test]
fn final_pareto_removes_dominated_frontier_and_keeps_first_equal_identity() {
    let start = datetime!(2026-09-21 10:00 UTC);
    let make = |id| {
        let movement = realized_ride(0, trip(id, "bus", &["origin", "destination"], &[1, 5]));
        let metrics =
            PathMetrics::from_movements(std::slice::from_ref(&movement), movement.arrival_at);
        let journey = RealizedJourney::from_movements(
            vec![
                JourneyAnchor {
                    kind: JourneyAnchorKind::Origin,
                    reached_at: start,
                },
                JourneyAnchor {
                    kind: JourneyAnchorKind::Destination,
                    reached_at: movement.arrival_at,
                },
            ],
            vec![movement],
            start,
        )
        .unwrap();
        EvaluatedCandidate { metrics, journey }
    };
    let mut dominated = make("a");
    dominated.metrics.bicycle_seconds = 10;
    let winner = make("z");
    let mut duplicate = winner.clone();
    duplicate.metrics.timing_reliability = TimingReliability::Fallback;
    let result = pareto_candidates(vec![dominated, winner.clone(), duplicate]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].journey, winner.journey);
    assert_eq!(result[0].metrics, winner.metrics);
}

#[test]
fn intermediate_state_pruning_matches_reference_for_capabilities_ties_and_permutations() {
    let states = pruning_state_corpus();
    let mut reversed = states.clone();
    reversed.reverse();
    let mut rotated = states.clone();
    rotated.rotate_left(3);
    let mut interleaved = states.clone();
    interleaved.sort_by_key(|state| state.ready_at.unix_timestamp_nanos() % 2);

    let unbounded = prune_states_reference(states.clone(), usize::MAX);
    assert!(unbounded.len() < states.len());
    assert_eq!(
        unbounded
            .iter()
            .filter(|state| state.active_bike_rental.is_some())
            .count(),
        3
    );

    for permutation in [states, reversed, rotated, interleaved] {
        for limit in [0, 1, 2, 4, 16] {
            let expected = prune_states_reference(permutation.clone(), limit);
            let actual = prune_states(permutation.clone(), limit);
            assert_eq!(actual, expected, "limit={limit}");
        }
    }
}

#[test]
fn frontier_cap_preserves_complete_journeys_and_ranked_journey_options() {
    let runtime = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 8);
    let corridor = candidate_corridor(&runtime, &query);
    let mut snapshot = TimingSnapshot::default();
    let mut departures = vec![
        trip("early", "bus", &["origin", "destination"], &[1, 5]),
        trip("middle", "bus", &["origin", "destination"], &[2, 6]),
        trip("late", "bus", &["origin", "destination"], &[3, 7]),
    ];
    departures[1].timing_basis = TimingBasis::Schedule;
    departures[2].timing_reliability = TimingReliability::Fallback;
    snapshot.insert(TransitPatternId::new("bus"), 0, departures);

    let optimized = evaluate(&runtime, &corridor, &snapshot, query.clone()).unwrap();
    let mut unpruned_query = query;
    unpruned_query.limits.max_candidates = usize::MAX;
    let reference = evaluate(&runtime, &corridor, &snapshot, unpruned_query).unwrap();
    assert_eq!(optimized, reference);

    let departure_at = datetime!(2026-09-21 10:00 UTC);
    let request = crate::api::PlanningRequest {
        origin: crate::api::SelectedPlace {
            coordinate: crate::domain::location::GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
            display_text: "Origin".into(),
        },
        destination: crate::api::SelectedPlace {
            coordinate: crate::domain::location::GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
            display_text: "Destination".into(),
        },
        selected_modes: vec![crate::api::PlanningMode::Bus],
        departure_setting: crate::api::DepartureSetting::DepartAt { time: departure_at },
    };
    let request: crate::domain::planning::PlanningInput = request.into();
    let context = super::super::super::model::JourneyPlanningContext::new(
        &request,
        departure_at,
        4 * 60 * 60,
        super::super::super::model::PlanningDeadline::disabled(),
    )
    .unwrap();
    let reconstructor = super::super::super::reconstruct::JourneyReconstructor::new(
        Arc::new(crate::street::StraightLineStreetRouter::default()),
        runtime,
        Arc::new(crate::domain::bikeshare::BikeShareNetwork::new(Vec::new())),
    );
    let ranked_options = |journeys: Vec<RealizedJourney>| {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let mut candidates = Vec::new();
                for journey in &journeys {
                    candidates.push(
                        reconstructor
                            .reconstruct(journey, &context)
                            .await
                            .unwrap()
                            .unwrap(),
                    );
                }
                let ranked = crate::domain::ranking::RankingPolicy::default().rank(
                    candidates
                        .iter()
                        .map(|candidate| candidate.ranking.clone())
                        .collect(),
                );
                let mut candidates_by_key = candidates
                    .into_iter()
                    .map(|candidate| (candidate.ranking.stable_key.clone(), candidate))
                    .collect::<HashMap<_, _>>();
                let options = ranked
                    .into_iter()
                    .enumerate()
                    .map(|(index, ranked)| {
                        let mut candidate = candidates_by_key.remove(&ranked.stable_key).unwrap();
                        super::super::super::reconstruct::assign_result_local_ids(
                            &mut candidate.option,
                            index,
                        );
                        candidate.option
                    })
                    .collect::<Vec<_>>();
                serde_json::to_value(options).unwrap()
            })
    };
    assert_eq!(ranked_options(optimized), ranked_options(reference));
}
