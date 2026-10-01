use std::sync::Arc;

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
        static_search::{StaticCorridor, generate},
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

fn query(modes: &[TransitMode], max_transfers: usize, max_corridors: usize) -> TransitQuery {
    TransitQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    )
    .with_allowed_transit_modes(modes.iter().copied())
    .with_limits(crate::routing::PlannerLimits {
        max_transfers,
        max_candidates: 16,
        max_phase2_expansions: 2_000,
        max_departures_per_query: 3,
        max_journey_duration_seconds: 4 * 60 * 60,
        ..crate::routing::PlannerLimits::default()
    })
    .with_max_static_corridors(max_corridors)
}

fn candidate_corridor(network: &TransitRuntime, query: &TransitQuery) -> RetainedCorridorSet {
    let topology = TransitTopologyIndex::new(network.data());
    let transfers = TransferShortcutIndex::from_network(network.data());
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

#[test]
fn physical_reboarding_counts_distinct_effective_trips_but_through_service_does_not() {
    let trip_a = trip("trip-a", "red-short", &["R10", "R09"], &[2, 4]);
    let trip_b = trip("trip-b", "red-full", &["R09", "R03"], &[7, 16]);
    let access_at = datetime!(2026-09-21 10:00 UTC);
    let access = AccessPoint::new(BoardingPointId::new("R10"), access_at, access_at, 0, 0);
    let egress = EgressPoint::new(BoardingPointId::new("destination"), 0, 0);
    let boarded_twice = EvaluationPath::from_access(access.clone())
        .with_ride(&trip_a, 0, 1)
        .with_ride(&trip_b, 0, 1);

    let reboarding = make_candidate(&boarded_twice, &egress, datetime!(2026-09-21 10:16 UTC));
    assert_eq!(reboarding.transfer_count, 1);

    let same_physical_trip = trip("through-trip", "red-short", &["R10", "R09"], &[2, 4]);
    let continued_trip = trip("through-trip", "red-full", &["R09", "R03"], &[4, 16]);
    let stayed_aboard = EvaluationPath::from_access(access)
        .with_ride(&same_physical_trip, 0, 1)
        .with_ride(&continued_trip, 0, 1);

    let through = make_candidate(&stayed_aboard, &egress, datetime!(2026-09-21 10:16 UTC));
    assert_eq!(through.transfer_count, 0);
}

#[test]
fn repeated_schedule_basis_does_not_prevent_direct_route_from_dominating_reboarding() {
    let start = datetime!(2026-09-21 10:00 UTC);
    let access = AccessPoint::new(BoardingPointId::new("R10"), start, start, 223, 0);
    let egress = EgressPoint::new(BoardingPointId::new("egress"), 116, 0);
    let terminal_walk = TransferShortcut::new(
        BoardingPointId::new("R03"),
        BoardingPointId::new("egress"),
        86,
        100,
    );
    let arrival = datetime!(2026-09-21 10:19:22 UTC);

    let mut direct_trip = trip("through", "red-full", &["R10", "R03"], &[2, 16]);
    direct_trip.timing_basis = TimingBasis::Schedule;
    direct_trip.timing_reliability = TimingReliability::Fallback;
    let direct_path = EvaluationPath::from_access(access.clone())
        .with_ride(&direct_trip, 0, 1)
        .with_walking_transfer(&terminal_walk);
    let direct_candidate = make_candidate(&direct_path, &egress, arrival);
    let direct_variant = make_variant(direct_candidate.clone(), &[]).unwrap();
    let direct = EvaluatedCandidate {
        journey_order_key: direct_candidate.deterministic_order_key(),
        variant_identity: direct_variant.identity(),
        variant: direct_variant,
        metrics: direct_candidate,
        bike_share_uses: 0,
        bicycle_seconds: 0,
    };

    let mut first_trip = trip("first", "red-short", &["R10", "R09"], &[2, 4]);
    first_trip.timing_basis = TimingBasis::Schedule;
    first_trip.timing_reliability = TimingReliability::Fallback;
    let mut second_trip = trip("second", "red-full", &["R09", "R03"], &[7, 16]);
    second_trip.timing_basis = TimingBasis::Schedule;
    second_trip.timing_reliability = TimingReliability::Fallback;
    let split_path = EvaluationPath::from_access(access)
        .with_ride(&first_trip, 0, 1)
        .with_ride(&second_trip, 0, 1)
        .with_walking_transfer(&terminal_walk);
    let split_candidate = make_candidate(&split_path, &egress, arrival);
    let split_variant = make_variant(split_candidate.clone(), &[]).unwrap();
    let split = EvaluatedCandidate {
        journey_order_key: split_candidate.deterministic_order_key(),
        variant_identity: split_variant.identity(),
        variant: split_variant,
        metrics: split_candidate,
        bike_share_uses: 0,
        bicycle_seconds: 0,
    };

    assert_eq!(direct.metrics.arrival_time, split.metrics.arrival_time);
    assert_eq!(
        direct.metrics.walking_seconds,
        split.metrics.walking_seconds
    );
    assert_eq!(direct.metrics.transfer_count, 0);
    assert_eq!(split.metrics.transfer_count, 1);
    assert!(direct.dominates(&split));
}

fn static_leg(pattern_id: &str, board: &str, alight: &str) -> StaticCorridorStep {
    StaticCorridorStep::Transit(StaticTransitLeg {
        pattern_id: TransitPatternId::new(pattern_id),
        board_stop_id: BoardingPointId::new(board),
        board_index: 0,
        alight_stop_id: BoardingPointId::new(alight),
        alight_index: 1,
        cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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

fn bike_shortcut(from_anchor: usize, to_anchor: usize, seconds: u32) -> BikeShortcut {
    BikeShortcut::new(
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

    let (candidates, stats) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:20 UTC)
    );
    assert_eq!(stats.corridors_feasible, 1);
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
            .skeleton()
            .movements()
            .get(2)
            .map(|movement| &movement.kind),
        Some(super::super::variant::JourneyMovementKind::Walking {
            kind: super::super::variant::WalkingMovementKind::Transfer,
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
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
    let bike = BikeShortcut::new(
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

    let (variants, stats) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    assert_eq!(variants.len(), 1);
    let timing = variants[0].evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:10 UTC));
    assert_eq!(timing.bike_share_uses, 1);
    assert_eq!(timing.walking_seconds, 150);
    let bike_shortcut = variants[0]
        .path()
        .iter()
        .find_map(|step| match &step.movement {
            super::super::variant::JourneyPathMovement::BikeShare { shortcut } => Some(shortcut),
            super::super::variant::JourneyPathMovement::Baseline { .. } => None,
        })
        .unwrap();
    assert_eq!(bike_shortcut.walk_to_pickup.duration_seconds, 60);
    assert_eq!(bike_shortcut.walk_from_dropoff.duration_seconds, 90);
    let bike_step = variants[0]
        .path()
        .iter()
        .find(|step| {
            matches!(
                step.movement,
                super::super::variant::JourneyPathMovement::BikeShare { .. }
            )
        })
        .unwrap();
    assert!(matches!(
        &variants[0].skeleton().anchors()[bike_step.from_anchor].kind,
        super::super::variant::JourneyAnchorKind::BoardingPoint { id }
            if id == &BoardingPointId::new("transfer-from")
    ));
    assert!(matches!(
        &variants[0].skeleton().anchors()[bike_step.to_anchor].kind,
        super::super::variant::JourneyAnchorKind::BoardingPoint { id }
            if id == &BoardingPointId::new("transfer-to")
    ));
    assert_eq!(stats.bike_realizations_selected, 1);
    assert!(variants[0].path().iter().any(|step| matches!(
        step.movement,
        super::super::variant::JourneyPathMovement::BikeShare { .. }
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
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
    let realizations = HashMap::from([(movement, vec![bike_shortcut(0, 1, 16 * 60)])]);

    let (variants, _) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    let bike_variant = variants
        .iter()
        .find(|variant| {
            variant.path().iter().any(|step| {
                matches!(
                    step.movement,
                    super::super::variant::JourneyPathMovement::BikeShare { .. }
                )
            })
        })
        .unwrap();
    assert_eq!(
        bike_variant.evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:27 UTC)
    );
    assert_eq!(
        bike_variant.transit_pattern_sequence(),
        vec![TransitPatternId::new("second")]
    );
    assert!(bike_variant.skeleton().movements().iter().any(|movement| {
        matches!(
            &movement.kind,
            super::super::variant::JourneyMovementKind::Transit { trip, .. }
                if trip.id == EffectiveTripId::new("second-17")
        )
    }));
}

#[test]
fn one_bike_shortcut_can_skip_multiple_transit_anchors() {
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
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
            vec![bike_shortcut(0, 1, 5 * 60)],
        ),
        (
            corridor_movement(0, 2, "origin", "destination"),
            vec![bike_shortcut(0, 2, 15 * 60)],
        ),
    ]);

    let (variants, stats) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    let cross_anchor = variants
        .iter()
        .find(|variant| {
            variant.transit_pattern_sequence().is_empty()
                && variant.path().iter().any(|step| {
                    matches!(
                        step.movement,
                        super::super::variant::JourneyPathMovement::BikeShare { .. }
                    )
                })
        })
        .unwrap();
    let timing = cross_anchor.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:15 UTC));
    assert_eq!(timing.bike_share_uses, 1);
    assert_eq!(stats.bike_exact_candidates_requested, 2);
    assert_eq!(stats.bike_continuous_extensions, 1);
}

#[test]
fn bike_can_replace_a_surface_walking_edge() {
    let network = network(Vec::new(), &["a", "b"], Vec::new());
    let query = TransitQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 600,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
        vec![bike_shortcut(0, 1, 300)],
    )]);

    let variants = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap()
    .0;

    assert!(variants.iter().any(|variant| {
        variant.path().iter().any(|step| {
            matches!(
                step.movement,
                super::super::variant::JourneyPathMovement::BikeShare { .. }
            )
        })
    }));
}

#[test]
fn slower_optimistic_bike_bound_skips_exact_routing() {
    let network = network(Vec::new(), &["a", "b"], Vec::new());
    let query = TransitQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 300,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
        vec![bike_shortcut(0, 1, 20 * 60)],
    )]);

    let (variants, stats) = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap();

    assert_eq!(stats.bike_lower_bound_checks, 1);
    assert_eq!(stats.bike_lower_bound_rejections, 1);
    assert_eq!(stats.bike_exact_candidates_requested, 0);
    assert!(
        variants
            .iter()
            .all(|variant| variant.path().iter().all(|step| !matches!(
                step.movement,
                super::super::variant::JourneyPathMovement::BikeShare { .. }
            )))
    );
}

#[test]
fn transit_waiting_time_is_included_when_screening_bike() {
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
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
        vec![bike_shortcut(0, 1, 16 * 60)],
    )]);

    let (variants, stats) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    assert_eq!(
        variants[0].evaluate_timing().unwrap().arrival_at,
        datetime!(2026-09-21 10:11 UTC)
    );
    assert_eq!(stats.bike_lower_bound_checks, 1);
    assert_eq!(stats.bike_exact_candidates_requested, 1);
    assert_eq!(stats.bike_realizations_feasible, 0);
}

#[test]
fn exact_bike_that_loses_to_walking_is_not_retained() {
    let network = network(Vec::new(), &["a", "b"], Vec::new());
    let query = TransitQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 12 * 60,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
        vec![bike_shortcut(0, 1, 14 * 60)],
    )]);

    let (variants, stats) = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap();

    assert_eq!(stats.bike_exact_candidates_requested, 1);
    assert_eq!(stats.bike_realizations_feasible, 0);
    assert!(
        variants
            .iter()
            .all(|variant| variant.path().iter().all(|step| !matches!(
                step.movement,
                super::super::variant::JourneyPathMovement::BikeShare { .. }
            )))
    );
}

#[test]
fn identical_physical_bike_movements_use_one_exact_realization() {
    let network = network(Vec::new(), &["a", "b"], Vec::new());
    let query = TransitQuery::new(
        vec![
            AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            ),
            AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                10,
            ),
        ],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 10 * 60,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
        vec![bike_shortcut(0, 1, 5 * 60)],
    )]);

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
    let query = TransitQuery::new(
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
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new(&stop_names[0]),
            egress_stop_id: BoardingPointId::new(stop_names.last().unwrap()),
            theoretical_duration_seconds: 12 * 60,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
                    vec![bike_shortcut(from, to, 20 * 60)],
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

    assert_eq!(stats.bike_lower_bound_checks, 12);
    assert_eq!(stats.bike_lower_bound_rejections, 12);
    assert_eq!(stats.bike_exact_candidates_requested, 0);
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
    let query = TransitQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("d"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    )
    .with_allowed_transit_modes([TransitMode::Bus, TransitMode::Metro])
    .with_limits(crate::routing::PlannerLimits {
        max_transfers: 2,
        max_candidates: 16,
        max_phase2_expansions: 2_000,
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
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
    let first_bike = BikeShortcut::new(
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
    let second_bike = bike_shortcut(2, 3, 5 * 60);
    let realizations = HashMap::from([
        (corridor_movement(0, 1, "a", "b"), vec![first_bike]),
        (corridor_movement(2, 3, "c", "d"), vec![second_bike]),
    ]);

    let (variants, stats) =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap();

    let split_rentals = variants
        .iter()
        .find(|variant| {
            variant
                .path()
                .iter()
                .filter(|step| {
                    matches!(
                        step.movement,
                        super::super::variant::JourneyPathMovement::BikeShare { .. }
                    )
                })
                .count()
                == 2
        })
        .unwrap_or_else(|| {
            panic!(
                "expected two rentals; stats={:?}, variants={:?}",
                stats,
                variants
                    .iter()
                    .map(|variant| variant.identity().canonical_signature())
                    .collect::<Vec<_>>()
            )
        });
    let timing = split_rentals.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:25 UTC));
    assert_eq!(timing.bike_share_uses, 2);
    let last_bike = split_rentals
        .path()
        .iter()
        .rev()
        .find(|step| {
            matches!(
                step.movement,
                super::super::variant::JourneyPathMovement::BikeShare { .. }
            )
        })
        .unwrap();
    assert!(matches!(
        &last_bike.movement,
        super::super::variant::JourneyPathMovement::BikeShare { shortcut }
            if shortcut.rental_wait_seconds == 5 * 60
    ));
}

#[test]
fn bike_with_no_pareto_benefit_is_pruned() {
    let network = network(Vec::new(), &["a", "b"], Vec::new());
    let query = TransitQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    );
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 60,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
            steps: vec![StaticCorridorStep::SurfaceTransfer {
                from: BoardingPointId::new("a"),
                to: BoardingPointId::new("b"),
                walking_duration_seconds: 60,
                walking_distance_metres: 100,
            }],
        }],
    );
    let bike = BikeShortcut::new(
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

    let variants = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &TimingSnapshot::default(),
        query,
        &realizations,
    )
    .unwrap()
    .0;

    assert!(
        variants
            .iter()
            .all(|variant| variant.path().iter().all(|step| !matches!(
                step.movement,
                super::super::variant::JourneyPathMovement::BikeShare { .. }
            )))
    );
}

#[test]
fn transit_remains_when_it_reaches_the_anchor_before_bike() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["a", "b"])],
        &["a", "b"],
        Vec::new(),
    );
    let query = TransitQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
    )
    .with_allowed_transit_modes([TransitMode::Bus]);
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![StaticCorridor {
            access_stop_id: BoardingPointId::new("a"),
            egress_stop_id: BoardingPointId::new("b"),
            theoretical_duration_seconds: 0,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
        vec![bike_shortcut(0, 1, 10 * 60)],
    )]);

    let variants =
        evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
            .unwrap()
            .0;

    assert_eq!(variants.len(), 1);
    assert!(
        variants[0]
            .transit_pattern_sequence()
            .contains(&TransitPatternId::new("bus"))
    );
    assert!(variants[0].path().iter().all(|step| !matches!(
        step.movement,
        super::super::variant::JourneyPathMovement::BikeShare { .. }
    )));
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
    let mut query = TransitQuery::new(
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
    );
    query.limits.max_candidates = 10;
    let corridor = RetainedCorridorSet::from_retained_for_test(
        &network,
        vec![
            StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 300,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![static_leg("walky", "a", "destination")],
            },
            StaticCorridor {
                access_stop_id: BoardingPointId::new("x"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 480,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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

    let variants = evaluate_with_test_bike_routes(
        &network,
        &corridor,
        &snapshot,
        query,
        &TestBikeRoutes::new(),
    )
    .unwrap()
    .0;

    assert_eq!(variants.len(), 2);
    let metrics = variants
        .iter()
        .map(|variant| variant.evaluate_timing().unwrap())
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

    let (candidates, stats) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.is_empty());
    assert_eq!(stats.infeasible_no_departure, 1);
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

    let (candidates, stats) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert!(candidates.is_empty());
    assert!(stats.malformed_trips > 0);
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

    assert_eq!(candidates.len(), 1);
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
            .skeleton()
            .movements()
            .get(1)
            .map(|movement| &movement.kind),
        Some(super::super::variant::JourneyMovementKind::Transit { .. })
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
        candidate.skeleton().movements().iter().any(|movement| {
            matches!(
                &movement.kind,
                super::super::variant::JourneyMovementKind::Transit { trip, .. }
                    if trip.timing_reliability == TimingReliability::Reliable
            )
        })
    }));
}

#[test]
fn phase2_state_pruning_keeps_reliability_alternatives_for_later_legs() {
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
        candidate.skeleton().movements().iter().any(|movement| {
            matches!(
                &movement.kind,
                super::super::variant::JourneyMovementKind::Transit { trip, .. }
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
            max_phase2_expansions: 2_000,
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
fn phase2_respects_candidate_limit_after_pareto_pruning() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
        &["origin", "destination"],
        Vec::new(),
    );
    let mut query = query(&[TransitMode::Bus], 0, 10);
    query.origin_access[0].walking_seconds = 200;
    query.origin_access.push(AccessPoint::new(
        BoardingPointId::new("origin"),
        datetime!(2026-09-21 10:00 UTC),
        datetime!(2026-09-21 10:03 UTC),
        100,
        0,
    ));
    query.origin_access.push(AccessPoint::new(
        BoardingPointId::new("origin"),
        datetime!(2026-09-21 10:00 UTC),
        datetime!(2026-09-21 10:05 UTC),
        0,
        0,
    ));
    query.limits.max_candidates = 2;
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![
            trip("early-arrival", "bus", &["origin", "destination"], &[1, 5]),
            trip("middle-arrival", "bus", &["origin", "destination"], &[4, 8]),
            trip("late-arrival", "bus", &["origin", "destination"], &[6, 10]),
        ],
    );

    let (candidates, stats) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(stats.final_candidates, 2);
    assert!(stats.candidates_before_pareto >= stats.final_candidates);
    assert_eq!(candidates.len(), 2);
}

#[test]
fn phase2_expansion_limit_rejects_partial_candidates() {
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
    query.limits.max_phase2_expansions = 1;
    let corridor = candidate_corridor(&network, &query);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus"),
        0,
        vec![trip("bus-trip", "bus", &["origin", "destination"], &[1, 5])],
    );

    assert!(evaluate_with_stats(&network, &corridor, &snapshot, query).is_err());
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
    assert_eq!(stats.corridors_feasible, 0);
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
    let alternatives = corridor.equivalent_legs_for(&network, retained_leg);
    assert_eq!(alternatives.len(), 2);
    let alternative = alternatives
        .iter()
        .find(|leg| leg.pattern_id != retained_leg.pattern_id)
        .unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        alternative.pattern_id.clone(),
        alternative.board_index,
        vec![trip(
            "equivalent-trip",
            alternative.pattern_id.as_str(),
            &["origin", "destination"],
            &[1, 10],
        )],
    );

    let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].transit_pattern_sequence(),
        vec![alternative.pattern_id.clone()]
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
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
            steps: vec![
                static_leg("a-bus-prefix", "origin", "middle"),
                static_leg("b-bus-suffix", "middle", "destination"),
            ],
        },
        StaticCorridor {
            access_stop_id: BoardingPointId::new("origin"),
            egress_stop_id: BoardingPointId::new("destination"),
            theoretical_duration_seconds: 2,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
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
    let query = query(&[TransitMode::Bus], 0, 1)
        .with_deadline(super::super::model::PlanningDeadline::elapsed_for_test());

    let result = evaluate_with_stats(&network, &corridor, &snapshot, query);

    assert_eq!(result.unwrap_err(), RoutingError::DeadlineExceeded);
}
