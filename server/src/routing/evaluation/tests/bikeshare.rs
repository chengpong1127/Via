use super::*;

#[test]
fn faster_bike_surface_transfer_catches_the_next_transit_departure() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["origin", "transfer-from"]),
            pattern(
                "second",
                TransitMode::Metro,
                &["transfer-to", "destination"],
            ),
        ],
        &["origin", "transfer-from", "transfer-to", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 1);
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 0,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("first", "origin", "transfer-from"),
                StaticCorridorStep::SurfaceTransfer {
                    from: BoardingPointId::new("transfer-from"),
                    to: BoardingPointId::new("transfer-to"),
                    walking_duration_seconds: 8 * 60,
                    walking_distance_metres: 100,
                },
                static_leg("second", "transfer-to", "destination"),
            ],
        }],
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("first"),
        0,
        vec![trip(
            "first-trip",
            "first",
            &["origin", "transfer-from"],
            &[0, 0],
        )],
    );
    snapshot.insert(
        TransitPatternId::new("second"),
        0,
        vec![trip(
            "second-trip",
            "second",
            &["transfer-to", "destination"],
            &[6, 10],
        )],
    );
    let movement = CorridorMovement {
        from_anchor: 1,
        to_anchor: 2,
        from: BoardingPointId::new("transfer-from"),
        to: BoardingPointId::new("transfer-to"),
    };
    let bike = BikeShareMovement::new(
        1,
        2,
        BikeStationId::new("pickup"),
        BikeStationId::new("dropoff"),
        BikeWalkingAccess::new(60, 50),
        crate::routing::BikeRide {
            duration_seconds: 200,
            distance_metres: 1_000,
        },
        BikeWalkingAccess::new(90, 50),
    )
    .unwrap();
    let realizations = HashMap::from([(movement, vec![bike])]);

    let (realized_journeys, _) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    assert_eq!(realized_journeys.len(), 1);
    let timing = realized_journeys[0].evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:10 UTC));
    assert_eq!(timing.bike_share_uses, 1);
    assert_eq!(timing.walking_seconds, 150);
    let bike_movement = realized_journeys[0]
        .movements()
        .iter()
        .find_map(|step| match &step.kind {
            super::super::super::journey::JourneyMovementKind::BikeShare {
                movement: bike_movement,
            } => Some(bike_movement),
            super::super::super::journey::JourneyMovementKind::Walking { .. }
            | super::super::super::journey::JourneyMovementKind::Transit { .. } => None,
        })
        .unwrap();
    assert_eq!(bike_movement.walk_to_pickup.duration_seconds, 60);
    assert_eq!(bike_movement.walk_from_dropoff.duration_seconds, 90);
    let bike_step = realized_journeys[0]
        .movements()
        .iter()
        .find(|step| {
            matches!(
                step.kind,
                super::super::super::journey::JourneyMovementKind::BikeShare { .. }
            )
        })
        .unwrap();
    assert!(matches!(
        &realized_journeys[0].anchors()[bike_step.from_anchor].kind,
        super::super::super::journey::JourneyAnchorKind::BoardingPoint { id }
            if id == &BoardingPointId::new("transfer-from")
    ));
    assert!(matches!(
        &realized_journeys[0].anchors()[bike_step.to_anchor].kind,
        super::super::super::journey::JourneyAnchorKind::BoardingPoint { id }
            if id == &BoardingPointId::new("transfer-to")
    ));
    assert!(realized_journeys[0].movements().iter().any(|step| matches!(
        step.kind,
        super::super::super::journey::JourneyMovementKind::BikeShare { .. }
    )));
}

#[test]
fn bike_replacing_a_transit_leg_requeries_the_next_departure() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["origin", "middle"]),
            pattern("second", TransitMode::Metro, &["middle", "destination"]),
        ],
        &["origin", "middle", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 1);
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 0,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("first", "origin", "middle"),
                static_leg("second", "middle", "destination"),
            ],
        }],
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("first"),
        0,
        vec![trip("first-trip", "first", &["origin", "middle"], &[1, 20])],
    );
    snapshot.insert(
        TransitPatternId::new("second"),
        0,
        vec![
            trip("second-17", "second", &["middle", "destination"], &[17, 27]),
            trip("second-25", "second", &["middle", "destination"], &[25, 35]),
        ],
    );
    let movement = corridor_movement(0, 1, "origin", "middle");
    let realizations = HashMap::from([(movement, vec![bike_movement(0, 1, 16 * 60)])]);

    let (realized_journeys, _) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    let bike_journey = realized_journeys
        .iter()
        .find(|journey| {
            journey.movements().iter().any(|step| {
                matches!(
                    step.kind,
                    super::super::super::journey::JourneyMovementKind::BikeShare { .. }
                )
            })
        })
        .unwrap();
    assert_eq!(
        bike_journey.evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:27 UTC)
    );
    assert_eq!(
        bike_journey.transit_pattern_sequence(),
        vec![TransitPatternId::new("second")]
    );
    assert!(bike_journey.movements().iter().any(|movement| {
        matches!(
            &movement.kind,
            super::super::super::journey::JourneyMovementKind::Transit { trip, .. }
                if trip.id == EffectiveTripId::new("second-17")
        )
    }));
}

#[test]
fn one_bike_movement_can_skip_multiple_transit_anchors() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["origin", "middle"]),
            pattern("second", TransitMode::Metro, &["middle", "destination"]),
        ],
        &["origin", "middle", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 1);
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 0,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("first", "origin", "middle"),
                static_leg("second", "middle", "destination"),
            ],
        }],
    );
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
            "second-trip",
            "second",
            &["middle", "destination"],
            &[40, 50],
        )],
    );
    let realizations = HashMap::from([
        (
            corridor_movement(0, 1, "origin", "middle"),
            vec![bike_movement(0, 1, 5 * 60)],
        ),
        (
            corridor_movement(0, 2, "origin", "destination"),
            vec![bike_movement(0, 2, 15 * 60)],
        ),
    ]);

    let (realized_journeys, stats) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    let cross_anchor = realized_journeys
        .iter()
        .find(|journey| {
            journey.transit_pattern_sequence().is_empty()
                && journey.movements().iter().any(|step| {
                    matches!(
                        step.kind,
                        super::super::super::journey::JourneyMovementKind::BikeShare { .. }
                    )
                })
        })
        .unwrap();
    let timing = cross_anchor.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:15 UTC));
    assert_eq!(timing.bike_share_uses, 1);
    assert_eq!(stats.bike_requests, 2);
}

#[test]
fn bike_can_replace_a_surface_walking_edge() {
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
            theoretical_duration_seconds: 600,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 600,
                walking_distance_metres: 100,
            }],
        }],
    );
    let realizations = HashMap::from([(
        corridor_movement(0, 1, "a", "b"),
        vec![bike_movement(0, 1, 300)],
    )]);

    let realized_journeys = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap()
    .0;

    assert!(realized_journeys.iter().any(|journey| {
        journey.movements().iter().any(|step| {
            matches!(
                step.kind,
                super::super::super::journey::JourneyMovementKind::BikeShare { .. }
            )
        })
    }));
}

#[test]
fn slower_bike_preserves_a_less_walking_tradeoff() {
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
            theoretical_duration_seconds: 300,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 300,
                walking_distance_metres: 100,
            }],
        }],
    );
    let realizations = HashMap::from([(
        corridor_movement(0, 1, "a", "b"),
        vec![bike_movement(0, 1, 20 * 60)],
    )]);

    let (realized_journeys, stats) = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap();

    assert_eq!(stats.bike_requests, 1);
    assert_eq!(realized_journeys.len(), 2);
    let timings = realized_journeys
        .iter()
        .map(|journey| journey.evaluate_timing().unwrap())
        .collect::<Vec<_>>();
    assert!(
        timings
            .iter()
            .any(|timing| timing.walking_seconds == 300 && timing.bike_share_uses == 0)
    );
    assert!(timings.iter().any(|timing| timing.walking_seconds == 0
        && timing.bike_share_uses == 1
        && timing.arrival_at == datetime!(2026-09-21 10:20 UTC)));
}

#[test]
fn transit_waiting_time_preserves_a_feasible_bike_alternative() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 4 * 60,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![static_leg("bus", "origin", "destination")],
        }],
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip(
            "bus-trip",
            "bus",
            &["origin", "destination"],
            &[7, 11],
        )],
    );
    let realizations = HashMap::from([(
        corridor_movement(0, 1, "origin", "destination"),
        vec![bike_movement(0, 1, 16 * 60)],
    )]);

    let (realized_journeys, stats) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    assert_eq!(
        realized_journeys[0].evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:11 UTC)
    );
    assert_eq!(stats.bike_requests, 1);
}

#[test]
fn slower_exact_bike_survives_with_less_walking() {
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
            theoretical_duration_seconds: 12 * 60,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 12 * 60,
                walking_distance_metres: 100,
            }],
        }],
    );
    let realizations = HashMap::from([(
        corridor_movement(0, 1, "a", "b"),
        vec![bike_movement(0, 1, 14 * 60)],
    )]);

    let (realized_journeys, stats) = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap();

    assert_eq!(stats.bike_requests, 1);
    assert_eq!(realized_journeys.len(), 2);
    let timings = realized_journeys
        .iter()
        .map(|journey| journey.evaluate_timing().unwrap())
        .collect::<Vec<_>>();
    assert!(
        timings
            .iter()
            .any(|timing| timing.walking_seconds == 720 && timing.bike_share_uses == 0)
    );
    assert!(timings.iter().any(|timing| timing.walking_seconds == 0
        && timing.bike_share_uses == 1
        && timing.arrival_at == datetime!(2026-09-21 10:14 UTC)));
}

#[test]
fn identical_physical_bike_movements_use_one_exact_realization() {
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
            theoretical_duration_seconds: 10 * 60,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 10 * 60,
                walking_distance_metres: 100,
            }],
        }],
    );
    let realizations = HashMap::from([(
        corridor_movement(0, 1, "a", "b"),
        vec![bike_movement(0, 1, 5 * 60)],
    )]);

    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![
            corridor.retained()[0].clone(),
            corridor.retained()[0].clone(),
        ],
    );
    let (_, stats) = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap();

    assert_eq!(stats.bike_requests, 1);
    assert_eq!(stats.bike_cache_hits, 1);
}

#[test]
fn separate_bike_rental_waits_until_fifteen_minutes_after_return() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["a", "b"]),
            pattern("second", TransitMode::Metro, &["b", "c"]),
            pattern("third", TransitMode::Bus, &["c", "d"]),
        ],
        &["a", "b", "c", "d"],
        Vec::new(),
    );
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
        vec![EgressPoint::new(BoardingPointId::new("d"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
        crate::config::ViaConfig::default().planner_limits(),
    )
    .with_allowed_transit_modes([TransitMode::Bus, TransitMode::Metro])
    .with_limits(crate::routing::PlannerLimits {
        max_transfers: 2,
        max_candidates: 16,
        max_departures_per_query: 3,
        max_journey_duration_seconds: 4 * 60 * 60,
        ..crate::routing::PlannerLimits::default()
    });
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("d"),
            theoretical_duration_seconds: 0,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("first", "a", "b"),
                static_leg("second", "b", "c"),
                static_leg("third", "c", "d"),
            ],
        }],
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("first"),
        0,
        vec![trip("first-trip", "first", &["a", "b"], &[1, 30])],
    );
    snapshot.insert(
        TransitPatternId::new("second"),
        0,
        vec![trip("second-trip", "second", &["b", "c"], &[11, 15])],
    );
    snapshot.insert(
        TransitPatternId::new("third"),
        0,
        vec![trip("third-trip", "third", &["c", "d"], &[30, 50])],
    );
    let first_bike = BikeShareMovement::new(
        0,
        1,
        BikeStationId::new("pickup-a"),
        BikeStationId::new("dropoff-b"),
        BikeWalkingAccess::new(0, 0),
        crate::routing::BikeRide {
            duration_seconds: 5 * 60,
            distance_metres: 1_000,
        },
        BikeWalkingAccess::new(5 * 60, 100),
    )
    .unwrap();
    let second_bike = bike_movement(2, 3, 5 * 60);
    let realizations = HashMap::from([
        (corridor_movement(0, 1, "a", "b"), vec![first_bike]),
        (corridor_movement(2, 3, "c", "d"), vec![second_bike]),
    ]);

    let (realized_journeys, stats) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    let split_rentals = realized_journeys
        .iter()
        .find(|journey| {
            journey
                .movements()
                .iter()
                .filter(|step| {
                    matches!(
                        step.kind,
                        super::super::super::journey::JourneyMovementKind::BikeShare { .. }
                    )
                })
                .count()
                == 2
        })
        .unwrap_or_else(|| {
            panic!(
                "expected two rentals; stats={:?}, realized_journeys={:?}",
                stats,
                realized_journeys
                    .iter()
                    .map(|journey| journey.signature())
                    .collect::<Vec<_>>()
            )
        });
    let timing = split_rentals.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:25 UTC));
    assert_eq!(timing.bike_share_uses, 2);
    let last_bike = split_rentals
        .movements()
        .iter()
        .rev()
        .find(|step| {
            matches!(
                step.kind,
                super::super::super::journey::JourneyMovementKind::BikeShare { .. }
            )
        })
        .unwrap();
    assert!(matches!(
        &last_bike.kind,
        super::super::super::journey::JourneyMovementKind::BikeShare { movement: bike_movement }
            if bike_movement.rental_wait_seconds == 5 * 60
    ));
}

#[test]
fn transit_remains_when_it_reaches_the_anchor_before_bike() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["a", "b"])],
        &["a", "b"],
        Vec::new(),
    );
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
    )
    .with_allowed_transit_modes([TransitMode::Bus]);
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 0,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![static_leg("bus", "a", "b")],
        }],
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip("bus-trip", "bus", &["a", "b"], &[1, 5])],
    );
    let realizations = HashMap::from([(
        corridor_movement(0, 1, "a", "b"),
        vec![bike_movement(0, 1, 10 * 60)],
    )]);

    let realized_journeys =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap()
            .0;

    assert_eq!(realized_journeys.len(), 2);
    assert!(realized_journeys[1].has_bike_share());
    assert!(
        realized_journeys[0]
            .transit_pattern_sequence()
            .contains(&TransitPatternId::new("bus"))
    );
    assert!(
        realized_journeys[0]
            .movements()
            .iter()
            .all(|step| !matches!(
                step.kind,
                super::super::super::journey::JourneyMovementKind::BikeShare { .. }
            ))
    );
}

#[tokio::test]
async fn walking_and_bike_share_both_compete_on_one_transit_edge() {
    let (result, calls) = evaluate_walking_replacement(20, None, None, false, true).await;
    assert_eq!(calls, 1);
    let journeys = &result.as_ref().unwrap().journeys;
    assert!(journeys.iter().any(|v| {
        v.movements().iter().any(|m| {
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
    assert!(
        journeys
            .iter()
            .any(|v| v.movements().iter().any(|s| matches!(
                s.kind,
                super::super::super::journey::JourneyMovementKind::BikeShare { .. }
            )))
    );
}

#[tokio::test]
async fn retained_walking_is_reused_while_bike_share_can_replace_it() {
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
    let retained = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 600,
            cost_source: crate::routing::corridors::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 600,
                walking_distance_metres: 1000,
            }],
        }],
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let street = MatrixCache::new(Arc::new(CountingWalkingRouter {
        calls: calls.clone(),
        error: None,
    }));
    let bike = TestBikeSource::new(HashMap::from([(
        corridor_movement(0, 1, "a", "b"),
        vec![bike_movement(0, 1, 240)],
    )]));
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let snapshot = TimingSnapshot::default();
    let result = evaluate_corridors(
        &network,
        &retained,
        &snapshot,
        &query,
        &street,
        Some(&bike),
        &[corridor],
    )
    .await
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(result.journeys.iter().any(|journey| {
        journey
            .movements()
            .iter()
            .any(|movement| matches!(movement.kind, JourneyMovementKind::BikeShare { .. }))
    }));
}

#[tokio::test]
async fn bike_actual_realization_enforces_query_time_limits() {
    for latest_arrival in [false, true] {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let mut query = query(&[TransitMode::Bus], 0, 1);
        let retained = candidate_corridor(&network, &query);
        if latest_arrival {
            query.latest_arrival = Some(query.departure_time + Duration::seconds(30));
        } else {
            query.limits.max_journey_duration_seconds = 30;
        }
        let corridor =
            EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
        let snapshot = TimingSnapshot::default();
        let bike = TestBikeSource::new(HashMap::from([(
            corridor_movement(0, 1, "origin", "destination"),
            vec![bike_movement(0, 1, 120)],
        )]));
        let street = fake_street();
        let result = evaluate_corridors(
            &network,
            &retained,
            &snapshot,
            &query,
            &street,
            Some(&bike),
            &[corridor],
        )
        .await
        .unwrap();
        assert!(result.journeys.is_empty());
        let mut stats = result.stats;
        bike.record_stats(&mut stats);
        assert_eq!(stats.bike_requests, 1);
        assert_eq!(stats.matrix_requests, 1);
        assert_eq!(stats.states_expanded, 2);
    }
}

#[tokio::test]
async fn all_corridors_complete_with_direct_walking_and_bike_share() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let direct = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let transit =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let mut bike = bike_movement(0, 1, 240);
    bike.from_anchor = 0;
    bike.to_anchor = 1;
    let street = MatrixCache::new(Arc::new(CountingWalkingRouter {
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        error: None,
    }));
    let bike = DirectBikePlanner { movement: bike };
    let snapshot = TimingSnapshot::default();
    let result = evaluate_corridors(
        &network,
        &retained,
        &snapshot,
        &query,
        &street,
        Some(&bike),
        &[direct, transit],
    )
    .await
    .unwrap();
    assert_eq!(result.stats.corridors_evaluated, 2);
    assert_eq!(result.journeys.len(), 3);
    assert!(
        result
            .journeys
            .iter()
            .filter(|journey| journey.anchors().len() == 2 && journey.movements().len() == 1)
            .count()
            == 2
    );
    assert!(result.journeys.iter().any(|journey| matches!(
        journey.movements()[0].kind,
        JourneyMovementKind::BikeShare { .. }
    )));
    assert!(result.journeys.iter().any(|journey| matches!(
        journey.movements()[0].kind,
        JourneyMovementKind::Walking { .. }
    )));
}

#[tokio::test]
async fn absent_bike_candidate_does_not_attempt_or_realize() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::from_retained_for_test(&network, Vec::new());
    let snapshot = TimingSnapshot::default();
    let query = query(&[], 0, 1);
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let street = fake_street();
    let bike = TestBikeSource::new(HashMap::new());
    let evaluator = CorridorEvaluator {
        network: &network,
        retained: &retained,
        timing: &snapshot,
        query: &query,
        street: &street,
        bike: Some(&bike),
    };
    let next = Run::new(&evaluator)
        .expand_state(&EvaluationState::initial(&query), &corridor, 0)
        .await
        .unwrap();
    assert!(next.iter().all(|state| !matches!(
        state.movements[0].kind,
        JourneyMovementKind::BikeShare { .. }
    )));
    let mut stats = CorridorEvaluationStats::default();
    bike.record_stats(&mut stats);
    assert_eq!((bike.calls(), stats.bike_requests), (1, 0));
}

#[test]
fn pure_bike_transitions_preserve_order_and_continue_one_rental() {
    let query = query(&[], 0, 1);
    let state = EvaluationState::initial(&query);
    let next = bike_successors(
        &query,
        &state,
        vec![bike_movement(0, 1, 60), bike_movement(0, 1, 120)],
        0,
    );
    assert_eq!(
        next.iter()
            .map(|s| (s.ready_at - query.departure_time).whole_seconds())
            .collect::<Vec<_>>(),
        vec![60, 120]
    );
    let continued = bike_successors(&query, &next[0], vec![bike_movement(0, 2, 180)], 1);
    assert_eq!(continued.len(), 1);
    assert_eq!(continued[0].movements.len(), 1);
    assert_eq!(continued[0].movements[0].from_anchor, 0);
    assert_eq!(continued[0].movements[0].to_anchor, 2);
    assert_eq!(
        continued[0].ready_at,
        query.departure_time + Duration::seconds(180)
    );
    assert!(bike_successors(&query, &state, Vec::new(), 0).is_empty());
    let mut limited = query.clone();
    limited.latest_arrival = Some(query.departure_time + Duration::seconds(59));
    assert!(bike_successors(&limited, &state, vec![bike_movement(0, 1, 60)], 0).is_empty());
    assert!(state.movements.is_empty());
}
