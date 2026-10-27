use std::{collections::HashMap, sync::Arc};

use time::{Duration, Time, macros::datetime};

use super::*;
use crate::{
    api::GeoCoordinate,
    domain::{
        bikeshare::BikeWalkingAccess,
        ids::{BikeStationId, EffectiveTripId, TransitPatternId},
        transit::{TransferEdge, TransitMode, TransitNetwork, TransitPattern, TransitStop},
        transit_runtime::TransitRuntime,
        transit_timing::{EffectiveStopTime, TimingBasis, TimingReliability},
    },
    routing::{
        AccessPoint, EgressPoint,
        static_search::{StaticCorridor, StaticCorridorStep, StaticCostSource, generate},
        topology::TransitTopologyIndex,
        transfers::TransferShortcutIndex,
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

fn static_leg(pattern_id: &str, board: &str, alight: &str) -> StaticCorridorStep {
    StaticCorridorStep::Transit(StaticTransitLeg {
        pattern_id: TransitPatternId::new(pattern_id),
        board_stop_id: BoardingPointId::new(board),
        board_index: 0,
        alight_stop_id: BoardingPointId::new(alight),
        alight_index: 1,
        cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
    })
}

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
        Some(super::super::realized::JourneyMovementKind::Walking {
            kind: super::super::realized::WalkingMovementKind::Transfer,
            ..
        })
    ));
}

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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
            super::super::realized::JourneyMovementKind::BikeShare {
                movement: bike_movement,
            } => Some(bike_movement),
            super::super::realized::JourneyMovementKind::Walking { .. }
            | super::super::realized::JourneyMovementKind::Transit { .. } => None,
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
                super::super::realized::JourneyMovementKind::BikeShare { .. }
            )
        })
        .unwrap();
    assert!(matches!(
        &realized_journeys[0].anchors()[bike_step.from_anchor].kind,
        super::super::realized::JourneyAnchorKind::BoardingPoint { id }
            if id == &BoardingPointId::new("transfer-from")
    ));
    assert!(matches!(
        &realized_journeys[0].anchors()[bike_step.to_anchor].kind,
        super::super::realized::JourneyAnchorKind::BoardingPoint { id }
            if id == &BoardingPointId::new("transfer-to")
    ));
    assert!(realized_journeys[0].movements().iter().any(|step| matches!(
        step.kind,
        super::super::realized::JourneyMovementKind::BikeShare { .. }
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
                    super::super::realized::JourneyMovementKind::BikeShare { .. }
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
            super::super::realized::JourneyMovementKind::Transit { trip, .. }
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
                        super::super::realized::JourneyMovementKind::BikeShare { .. }
                    )
                })
        })
        .unwrap();
    let timing = cross_anchor.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:15 UTC));
    assert_eq!(timing.bike_share_uses, 1);
    assert_eq!(stats.bike_exact_candidates_requested, 2);
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
                super::super::realized::JourneyMovementKind::BikeShare { .. }
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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

    assert_eq!(stats.bike_exact_candidates_requested, 1);
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
    assert_eq!(stats.bike_exact_candidates_requested, 1);
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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

    assert_eq!(stats.bike_exact_candidates_requested, 1);
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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

    assert_eq!(stats.bike_exact_candidates_requested, 1);
    assert_eq!(stats.bike_exact_cache_hits, 1);
}

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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
                        super::super::realized::JourneyMovementKind::BikeShare { .. }
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
                    .map(|journey| journey.identity().canonical_signature())
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
                super::super::realized::JourneyMovementKind::BikeShare { .. }
            )
        })
        .unwrap();
    assert!(matches!(
        &last_bike.kind,
        super::super::realized::JourneyMovementKind::BikeShare { movement: bike_movement }
            if bike_movement.rental_wait_seconds == 5 * 60
    ));
}

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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
                super::super::realized::JourneyMovementKind::BikeShare { .. }
            ))
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
                cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
                steps: vec![static_leg("walky", "a", "destination")],
            },
            StaticCorridor {
                access_stop_id: BoardingPointId::new("x"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 480,
                cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
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
        Some(super::super::realized::JourneyMovementKind::Transit { .. })
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
                super::super::realized::JourneyMovementKind::Transit { trip, .. }
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
                super::super::realized::JourneyMovementKind::Transit { trip, .. }
                    if trip.id == EffectiveTripId::new("z-reliable")
            )
        })
    }));
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
fn phase2_completes_prepared_candidates_without_a_work_budget() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 10);
    query.origin_access.push(AccessPoint::new(
        BoardingPointId::new("origin"),
        datetime!(2026-09-21 10:00 UTC),
        datetime!(2026-09-21 10:00 UTC),
        0,
        0,
    ));
    query.deadline = crate::routing::PlanningDeadline::disabled();
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip("bus-trip", "bus", &["origin", "destination"], &[1, 5])],
    );

    assert!(
        !evaluate_with_stats(&network, &corridor, &snapshot, query)
            .unwrap()
            .0
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
fn expired_deadline_stops_phase2_before_evaluating_corridors() {
    let network = network(Vec::new(), &["origin", "destination"], Vec::new());
    let corridor = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let mut query = query(&[TransitMode::Bus], 0, 1);
    query.deadline = super::super::model::PlanningDeadline::elapsed_for_test();

    let result = evaluate_with_stats(&network, &corridor, &snapshot, query);

    assert_eq!(result.unwrap_err(), RoutingError::DeadlineExceeded);
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
        assert!(query.constraints.time.is_some());
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
) -> (EvaluationResult, usize) {
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
    let street = Arc::new(CountingWalkingRouter {
        calls: calls.clone(),
        error,
    });
    let mut dependencies = TestDependencies {
        walking: WalkingCache::new(street),
        bike_share: None,
    };
    if with_bike {
        dependencies.bike_share = Some(BikeShareSession::for_test(Box::new(TestBikePlanner {
            routes: HashMap::from([(
                corridor_movement(0, 1, "origin", "destination"),
                vec![bike_movement(0, 1, 240)],
            )]),
            cache: Default::default(),
            hits: Default::default(),
        })));
    }
    let make_corridor =
        || EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let corridors = if duplicate {
        vec![make_corridor(), make_corridor()]
    } else {
        vec![make_corridor()]
    };
    let result = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &dependencies.walking,
        dependencies.bike_share.as_ref(),
    )
    .evaluate(&corridors)
    .await
    .unwrap();
    (result, calls.load(std::sync::atomic::Ordering::SeqCst))
}

#[tokio::test]
async fn competitive_walking_replaces_transit_with_matrix_metrics() {
    let (result, calls) = evaluate_walking_replacement(20, None, None, false, false).await;
    assert_eq!(calls, 1);
    assert!(result.journeys.iter().any(|v| {
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
    let (result, calls) = evaluate_walking_replacement(1, None, None, false, false).await;
    assert_eq!(calls, 1);
    assert_eq!(result.journeys.len(), 2);
    assert!(result.journeys[1].transit_pattern_sequence().is_empty());
    assert_eq!(
        result.journeys[0].transit_pattern_sequence(),
        vec![TransitPatternId::new("bus")]
    );
}

#[tokio::test]
async fn optional_walking_dependency_failure_preserves_transit() {
    let (result, calls) = evaluate_walking_replacement(
        20,
        Some(crate::street::StreetRoutingError::Timeout),
        None,
        false,
        false,
    )
    .await;
    assert_eq!(calls, 1);
    assert_eq!(result.journeys.len(), 1);
    assert!(result.failures.transient);
    assert_eq!(
        result.journeys[0].transit_pattern_sequence(),
        vec![TransitPatternId::new("bus")]
    );
}

#[tokio::test]
async fn repeated_corridor_pairs_reuse_exact_walking_routes() {
    let (result, calls) = evaluate_walking_replacement(20, None, None, true, false).await;
    assert_eq!(calls, 1);
    assert!(!result.journeys.is_empty());
}

#[tokio::test]
async fn walking_replacement_respects_latest_arrival() {
    let (result, calls) = evaluate_walking_replacement(20, None, Some(299), false, false).await;
    assert!(result.journeys.is_empty());
    assert_eq!(calls, 1);
    assert_eq!(result.stats.walking_matrix_requests, 1);
    assert_eq!(
        (result.stats.transit_operations_attempted
            + result.stats.walking_operations_attempted
            + result.stats.bike_operations_attempted),
        3
    );
}

#[tokio::test]
async fn walking_and_bike_share_both_compete_on_one_transit_edge() {
    let (result, calls) = evaluate_walking_replacement(20, None, None, false, true).await;
    assert_eq!(calls, 1);
    assert!(result.journeys.iter().any(|v| {
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
        result
            .journeys
            .iter()
            .any(|v| v.movements().iter().any(|s| matches!(
                s.kind,
                super::super::realized::JourneyMovementKind::BikeShare { .. }
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
            cost_source: crate::routing::static_search::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 600,
                walking_distance_metres: 1000,
            }],
        }],
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dependencies = TestDependencies {
        walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: calls.clone(),
            error: None,
        })),
        bike_share: Some(BikeShareSession::for_test(Box::new(TestBikePlanner {
            routes: HashMap::from([(
                corridor_movement(0, 1, "a", "b"),
                vec![bike_movement(0, 1, 240)],
            )]),
            cache: Default::default(),
            hits: Default::default(),
        }))),
    };
    let corridor =
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap();
    let snapshot = TimingSnapshot::default();
    let result = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &dependencies.walking,
        dependencies.bike_share.as_ref(),
    )
    .evaluate(&[corridor])
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
        let planners = TestDependencies {
            bike_share: Some(BikeShareSession::for_test(Box::new(TestBikePlanner {
                routes: HashMap::from([(
                    corridor_movement(0, 1, "origin", "destination"),
                    vec![bike_movement(0, 1, 120)],
                )]),
                cache: Default::default(),
                hits: Default::default(),
            }))),
            ..test_dependencies()
        };
        let result = FixedCorridorEvaluator::new(
            &network,
            &retained,
            &snapshot,
            &query,
            &planners.walking,
            planners.bike_share.as_ref(),
        )
        .evaluate(&[corridor])
        .await
        .unwrap();
        assert!(result.journeys.is_empty());
        assert_eq!(result.stats.bike_exact_candidates_requested, 1);
        assert_eq!(result.stats.walking_matrix_requests, 1);
        assert_eq!(
            (result.stats.transit_operations_attempted
                + result.stats.walking_operations_attempted
                + result.stats.bike_operations_attempted),
            4
        );
    }
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
    let planners = TestDependencies {
        walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: None,
        })),
        bike_share: None,
    };
    let result = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    )
    .evaluate(&[corridor])
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
    let mut evaluator = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    );
    let result = evaluator.evaluate(&[corridor]).await;
    assert!(!result.unwrap().journeys.is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(evaluator.stats.corridors_completed, 1);
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
        [CorridorEdge {
            planned_movement: PlannedMovement::WalkingToResolve
        }]
    ));
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
        let mut latencies = Vec::new();
        let mut last_stats = Phase2EvaluationStats::default();
        for _ in 0..100 {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let planners = TestDependencies {
                walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
                    calls: calls.clone(),
                    error: None,
                })),
                bike_share: None,
            };
            let mut corridors = vec![
                EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query)
                    .unwrap(),
            ];
            if include_direct {
                corridors.push(EvaluationCorridor::direct_walking(
                    query.origin,
                    query.destination,
                ));
            }
            let result = FixedCorridorEvaluator::new(
                &network,
                &retained,
                &snapshot,
                &query,
                &planners.walking,
                planners.bike_share.as_ref(),
            )
            .evaluate(&corridors)
            .await
            .unwrap();
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                if include_direct { 2 } else { 1 }
            );
            if include_direct {
                assert!(result.journeys.iter().any(|j| j.anchors().len() == 2));
                assert!(
                    result
                        .journeys
                        .iter()
                        .any(|j| !j.transit_pattern_sequence().is_empty())
                );
            }
            latencies.push(result.stats.phase2_latency_us);
            last_stats = result.stats;
        }
        latencies.sort_unstable();
        eprintln!(
            "direct={include_direct}: P2 median={}us p95={}us states_expanded={} walking_calls={}",
            latencies[50],
            latencies[95],
            last_stats.states_expanded,
            if include_direct { 2 } else { 1 }
        );
        measurements.push(last_stats);
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

#[tokio::test]
async fn walking_cache_reuses_exact_requests_but_keeps_departure_times_distinct() {
    let query = query(&[], 0, 1);
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
        calls: calls.clone(),
        error: None,
    }));
    let from = &corridor.anchors()[0];
    let to = &corridor.anchors()[1];
    let first = walking
        .realize(from, to, query.departure_time)
        .await
        .unwrap();
    let cached = walking
        .realize(from, to, query.departure_time)
        .await
        .unwrap();
    assert_eq!(first, cached);
    walking
        .realize(from, to, query.departure_time + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(walking.requests(), 2);
    assert_eq!(walking.cache_hits(), 1);
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
    let planners = TestDependencies {
        walking: WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: None,
        })),
        bike_share: Some(BikeShareSession::for_test(Box::new(DirectBikePlanner {
            movement: bike,
        }))),
    };
    let snapshot = TimingSnapshot::default();
    let result = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    )
    .evaluate(&[direct, transit])
    .await
    .unwrap();
    assert_eq!(result.stats.corridors_completed, 2);
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

struct DirectBikePlanner {
    movement: BikeShareMovement,
}

#[async_trait::async_trait]
impl TestBikeProvider for DirectBikePlanner {
    fn has_candidate(&self, request: &BikeMovementRequest<'_>) -> bool {
        matches!(request.from.kind, JourneyAnchorKind::Origin)
            && matches!(request.to.kind, JourneyAnchorKind::Destination)
    }
    async fn realize(
        &self,
        _: &BikeMovementRequest<'_>,
    ) -> Result<Vec<BikeShareMovement>, crate::street::StreetRoutingError> {
        Ok(vec![self.movement.clone()])
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
    let mut evaluator = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    );
    assert!(matches!(
        evaluator.evaluate(&corridors).await,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(evaluator.stats.corridors_completed, 1);
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
    let mut evaluator = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &planners.walking,
        planners.bike_share.as_ref(),
    );
    let result = evaluator.evaluate(&[corridor]).await.unwrap();
    assert_eq!(result.journeys.len(), 2);
    assert_eq!(result.stats.states_expanded, 1);
    assert_eq!(result.stats.successors_generated, 2);
    assert_eq!(result.stats.walking_operations_attempted, 1);
    assert_eq!(result.stats.bike_operations_attempted, 1);
    assert_eq!(result.stats.corridors_completed, 1);
    assert_eq!(result.stats.walking_matrix_requests, 1);
}

#[tokio::test]
async fn unreachable_walking_matrix_has_no_successor_or_provider_failure() {
    let network = network(Vec::new(), &[], Vec::new());
    let query = query(&[], 0, 1);
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    let dependencies = test_dependencies();
    let result = FixedCorridorEvaluator::new(
        &network,
        &retained,
        &snapshot,
        &query,
        &dependencies.walking,
        None,
    )
    .evaluate(&[EvaluationCorridor::direct_walking(
        query.origin,
        query.destination,
    )])
    .await
    .unwrap();
    assert!(result.journeys.is_empty());
    assert!(!result.failures.internal && !result.failures.transient);
    assert_eq!(result.stats.walking_matrix_requests, 1);
    assert_eq!(
        (result.stats.transit_operations_attempted
            + result.stats.walking_operations_attempted
            + result.stats.bike_operations_attempted),
        1
    );
}

#[tokio::test]
async fn walking_uses_actual_matrix_timing_even_when_geometry_bound_exceeds_query_limit() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::from_retained_for_test(&network, Vec::new());
    let snapshot = TimingSnapshot::default();
    let origin = crate::street::Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = crate::street::Coordinate::try_new(25.02, 121.5).unwrap();
    let corridor = EvaluationCorridor::direct_walking(origin, destination);
    for latest_arrival in [false, true] {
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
            if latest_arrival {
                query.latest_arrival = Some(query.departure_time + Duration::seconds(30));
            } else {
                query.limits.max_journey_duration_seconds = 30;
            }
            let walking = WalkingCache::new(Arc::new(
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
            let result =
                FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None)
                    .evaluate(std::slice::from_ref(&corridor))
                    .await
                    .unwrap();
            assert_eq!(result.journeys.len(), usize::from(actual_seconds <= 30));
            assert_eq!(result.stats.walking_matrix_requests, 1);
            assert_eq!(
                (result.stats.transit_operations_attempted
                    + result.stats.walking_operations_attempted
                    + result.stats.bike_operations_attempted),
                1
            );
        }
    }
}

#[tokio::test]
async fn absent_bike_candidate_does_not_attempt_or_realize() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::from_retained_for_test(&network, Vec::new());
    let snapshot = TimingSnapshot::default();
    let query = query(&[], 0, 1);
    let corridor = EvaluationCorridor::direct_walking(query.origin, query.destination);
    let walking = test_dependencies().walking;
    let bike = BikeShareSession::for_test(Box::new(TestBikePlanner {
        routes: HashMap::new(),
        cache: Default::default(),
        hits: Default::default(),
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
    let next = outcome.successors.unwrap();
    assert!(next.iter().all(|state| !matches!(
        state.movements[0].kind,
        JourneyMovementKind::BikeShare { .. }
    )));
    assert_eq!(
        (outcome.stats.transit_operations_attempted
            + outcome.stats.walking_operations_attempted
            + outcome.stats.bike_operations_attempted),
        1
    );
    assert_eq!(bike.realization_stats().exact_requests, 0);
}

#[tokio::test]
#[ignore = "deterministic Phase A comparison"]
async fn phase_a_deterministic_benchmark() {
    let network = network(Vec::new(), &[], Vec::new());
    let retained = RetainedCorridorSet::default();
    let snapshot = TimingSnapshot::default();
    for count in [32, 2400] {
        let query = query(&[], 0, 1);
        let corridors = (0..count)
            .map(|_| EvaluationCorridor::direct_walking(query.origin, query.destination))
            .collect::<Vec<_>>();
        let mut samples = Vec::new();
        let mut expected = None;
        for iteration in 0..25 {
            let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                error: None,
            }));
            let result =
                FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None)
                    .evaluate(&corridors)
                    .await
                    .unwrap();
            let fingerprints = result
                .journeys
                .iter()
                .map(|j| j.identity().canonical_signature())
                .collect::<Vec<_>>();
            if let Some(expected) = &expected {
                assert_eq!(&fingerprints, expected);
            } else {
                expected = Some(fingerprints);
            }
            if iteration >= 5 {
                samples.push(result.stats.phase2_latency_us);
            }
            if iteration == 24 {
                eprintln!(
                    "phase_a count={count} stats={:?} fingerprints={expected:?}",
                    result.stats
                );
            }
        }
        samples.sort_unstable();
        eprintln!(
            "phase_a count={count} median_us={} p95_us={}",
            samples[10], samples[19]
        );
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
        let next = expansion::walking_next_state(
            &query,
            &state,
            expansion::ResolvedWalking {
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

#[test]
fn pure_bike_transitions_preserve_order_and_continue_one_rental() {
    let query = query(&[], 0, 1);
    let state = EvaluationState::initial(&query);
    let next = expansion::bike_next_states(
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
    let continued =
        expansion::bike_next_states(&query, &next[0], vec![bike_movement(0, 2, 180)], 1);
    assert_eq!(continued.len(), 1);
    assert_eq!(continued[0].movements.len(), 1);
    assert_eq!(continued[0].movements[0].from_anchor, 0);
    assert_eq!(continued[0].movements[0].to_anchor, 2);
    assert_eq!(
        continued[0].ready_at,
        query.departure_time + Duration::seconds(180)
    );
    assert!(expansion::bike_next_states(&query, &state, Vec::new(), 0).is_empty());
    let mut limited = query.clone();
    limited.latest_arrival = Some(query.departure_time + Duration::seconds(59));
    assert!(
        expansion::bike_next_states(&limited, &state, vec![bike_movement(0, 1, 60)], 0).is_empty()
    );
    assert!(state.movements.is_empty());
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
        FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None)
            .evaluate(&corridors),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.stats.corridors_attempted, 2400);
    assert_eq!(result.stats.corridors_completed, 2400);
    assert_eq!(result.stats.states_expanded, 2400);
    assert_eq!(result.stats.walking_operations_attempted, 2400);
    assert_eq!(result.stats.successors_generated, 2400);
    assert_eq!(result.stats.walking_matrix_requests, 1);
    assert_eq!(result.stats.walking_cache_hits, 2399);
    assert_eq!(result.journeys.len(), 1);
}

fn expansion_inputs<'a>(
    network: &'a TransitRuntime,
    retained: &'a RetainedCorridorSet,
    snapshot: &'a TimingSnapshot,
    query: &'a PlanningQuery,
) -> expansion::ExpansionInputs<'a> {
    expansion::ExpansionInputs {
        network,
        corridor: retained,
        snapshot,
        query,
    }
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
        let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: calls.clone(),
            error,
        }));
        let resolver = expansion::ExpansionResolver {
            inputs: expansion_inputs(&network, &retained, &snapshot, &query),
            walking: &walking,
            bike_share: None,
        };
        for _ in 0..2 {
            let outcome = resolver
                .resolve_walking(
                    &EvaluationState::initial(&query),
                    &corridor,
                    &corridor.edges()[0],
                    0,
                )
                .await;
            assert_eq!(
                outcome.stats,
                expansion::ExpansionStats {
                    walking_operations_attempted: 1,
                    successors_generated: usize::from(succeeds),
                    ..Default::default()
                }
            );
            assert_eq!(
                (outcome.failures.transient, outcome.failures.internal),
                (transient, internal)
            );
            assert_eq!(outcome.successors.unwrap().len(), usize::from(succeeds));
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(resolver.walking.cache_hits(), 1);
    }
}

struct OutcomeBikeProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    error: Option<crate::street::StreetRoutingError>,
    expire_deadline: bool,
}

#[async_trait::async_trait]
impl TestBikeProvider for OutcomeBikeProvider {
    fn has_candidate(&self, _: &BikeMovementRequest<'_>) -> bool {
        true
    }
    async fn realize(
        &self,
        request: &BikeMovementRequest<'_>,
    ) -> Result<Vec<BikeShareMovement>, crate::street::StreetRoutingError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.expire_deadline {
            tokio::time::advance(std::time::Duration::from_secs(2)).await;
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(vec![bike_movement(
            request.from_anchor,
            request.to_anchor,
            60,
        )])
    }
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
    let mut evaluator =
        FixedCorridorEvaluator::new(&network, &retained, &snapshot, &query, &walking, None);
    assert!(matches!(
        evaluator.evaluate(&[corridor]).await,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(evaluator.stats.states_expanded, 2);
    assert_eq!(evaluator.stats.walking_operations_attempted, 2);
    assert_eq!(evaluator.stats.transit_operations_attempted, 1);
    assert_eq!(evaluator.stats.transit_departure_lookups, 1);
    assert_eq!(evaluator.stats.successors_generated, 3);
    assert_eq!(evaluator.stats.corridors_completed, 0);
}

#[tokio::test]
#[ignore = "deterministic outcome cleanup comparison"]
async fn outcome_cleanup_multimodal_benchmark() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let query = query(&[TransitMode::Bus], 0, 1);
    let retained = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early", "bus", &["origin", "destination"], &[1, 2]),
            trip("later", "bus", &["origin", "destination"], &[2, 3]),
        ],
    );
    let corridors = vec![
        EvaluationCorridor::direct_walking(query.origin, query.destination),
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap(),
        EvaluationCorridor::from_transit(&network, &retained.retained()[0], &query).unwrap(),
    ];
    let mut samples = Vec::new();
    let mut expected = None;
    for iteration in 0..25 {
        let walking = WalkingCache::new(Arc::new(CountingWalkingRouter {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: None,
        }));
        let bike = BikeShareSession::for_test(Box::new(TestBikePlanner {
            routes: HashMap::from([(
                corridor_movement(0, 1, "origin", "destination"),
                vec![bike_movement(0, 1, 60), bike_movement(0, 1, 240)],
            )]),
            cache: Default::default(),
            hits: Default::default(),
        }));
        let result = FixedCorridorEvaluator::new(
            &network,
            &retained,
            &snapshot,
            &query,
            &walking,
            Some(&bike),
        )
        .evaluate(&corridors)
        .await
        .unwrap();
        let fingerprints = result
            .journeys
            .iter()
            .map(|j| {
                format!(
                    "{}@{:?}",
                    j.identity().canonical_signature(),
                    j.movements()
                        .iter()
                        .map(|m| (
                            m.departure_at.unix_timestamp_nanos(),
                            m.arrival_at.unix_timestamp_nanos()
                        ))
                        .collect::<Vec<_>>()
                )
            })
            .collect::<Vec<_>>();
        let mut work = result.stats;
        work.phase2_latency_us = 0;
        if let Some(expected) = &expected {
            assert_eq!(&(work, fingerprints.clone()), expected);
        } else {
            expected = Some((work, fingerprints.clone()));
        }
        if iteration >= 5 {
            samples.push(result.stats.phase2_latency_us);
        }
        if iteration == 24 {
            eprintln!(
                "cleanup_multimodal stats={:?} fingerprints={fingerprints:?}",
                result.stats
            );
        }
    }
    samples.sort_unstable();
    eprintln!(
        "cleanup_multimodal median_us={} p95_us={}",
        samples[10], samples[19]
    );
}

#[path = "parallel_tests.rs"]
mod parallel_tests;
