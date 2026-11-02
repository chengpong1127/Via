use std::collections::HashSet;

use time::{Time, Weekday, macros::datetime};

use super::*;
use crate::{
    domain::location::GeoCoordinate,
    domain::{
        ids::{BoardingPointId, TransitPatternId, TransitTripId},
        transit::{
            ScheduledStopTime, TransferEdge, TransitMode, TransitNetwork, TransitPattern,
            TransitStop, TransitTrip,
        },
        transit_runtime::TransitRuntime,
    },
    routing::{
        AccessPoint, EgressPoint, TransferShortcutIndex, TransitTopologyIndex, model::PlanningQuery,
    },
};

fn id(value: &str) -> BoardingPointId {
    BoardingPointId::new(value)
}

fn pattern(value: &str, mode: TransitMode, pattern: &[&str]) -> TransitPattern {
    TransitPattern::fixture(
        TransitPatternId::new(value),
        mode,
        value,
        pattern.last().copied().unwrap_or_default(),
        pattern.iter().map(|stop| id(stop)).collect(),
    )
}

fn trip(value: &str, pattern_id: &str, offsets: &[u32]) -> TransitTrip {
    TransitTrip::fixture_weekdays(
        TransitTripId::new(value),
        TransitPatternId::new(pattern_id),
        vec![
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
            Weekday::Saturday,
            Weekday::Sunday,
        ],
        offsets
            .iter()
            .map(|seconds| ScheduledStopTime::new(*seconds, *seconds))
            .collect(),
    )
}

fn network(patterns: Vec<TransitPattern>, trips: Vec<TransitTrip>) -> TransitNetwork {
    let stop_ids = patterns
        .iter()
        .flat_map(|pattern| pattern.stops.iter().cloned())
        .collect::<HashSet<_>>();
    TransitNetwork::fixture(
        Time::from_hms(4, 0, 0).unwrap(),
        stop_ids
            .into_iter()
            .enumerate()
            .map(|(index, stop_id)| {
                TransitStop::new(
                    stop_id.clone(),
                    stop_id.as_str(),
                    GeoCoordinate {
                        latitude: 25.0 + index as f64 * 0.00001,
                        longitude: 121.5,
                    },
                )
            })
            .collect(),
        patterns,
        trips,
    )
}

fn query(
    origin: &str,
    destination: &str,
    modes: &[TransitMode],
    max_transfers: usize,
    k: usize,
) -> PlanningQuery {
    PlanningQuery::new(
        crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
        crate::street::Coordinate::try_new(25.001, 121.501).unwrap(),
        crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
        vec![AccessPoint::new(
            id(origin),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(id(destination), 0, 0)],
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
    .with_max_static_corridors(k)
}

fn run(network: &TransitNetwork, query: &PlanningQuery) -> RetainedCorridorSet {
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));
    let topology = TransitTopologyIndex::new(runtime.network());
    let transfers = TransferShortcutIndex::from_network(runtime.network());
    generate(&runtime, &topology, &transfers, query).0
}

#[test]
fn consecutive_patterns_on_the_same_route_are_rejected() {
    let first = pattern("red-a", TransitMode::Metro, &["o", "hub"]);
    let mut second = pattern("red-b", TransitMode::Metro, &["hub", "d"]);
    second.route_id = first.route_id.clone();
    let network = network(
        vec![first, second],
        vec![
            trip("first-trip", "red-a", &[0, 100]),
            trip("second-trip", "red-b", &[0, 100]),
        ],
    );

    let corridors = run(&network, &query("o", "d", &[TransitMode::Metro], 1, 10));

    assert!(corridors.is_empty());
}

#[test]
fn surface_transfer_does_not_allow_reboarding_the_same_route() {
    let first = pattern("red-a", TransitMode::Metro, &["o", "alight"]);
    let mut second = pattern("red-b", TransitMode::Metro, &["board", "d"]);
    second.route_id = first.route_id.clone();
    let network = network(
        vec![first, second],
        vec![
            trip("first-trip", "red-a", &[0, 100]),
            trip("second-trip", "red-b", &[0, 100]),
        ],
    )
    .with_transfer_edges(vec![TransferEdge::new(id("alight"), id("board"), 30, 100)]);

    let corridors = run(&network, &query("o", "d", &[TransitMode::Metro], 1, 10));

    assert!(corridors.is_empty());
}

#[test]
fn consecutive_different_routes_of_the_same_mode_are_allowed() {
    for mode in [TransitMode::Metro, TransitMode::Bus] {
        let network = network(
            vec![
                pattern("first", mode, &["o", "hub"]),
                pattern("second", mode, &["hub", "d"]),
            ],
            vec![
                trip("first-trip", "first", &[0, 100]),
                trip("second-trip", "second", &[0, 100]),
            ],
        );

        let corridors = run(&network, &query("o", "d", &[mode], 1, 10));

        assert_eq!(corridors.retained().len(), 1);
        assert_eq!(
            corridors.retained()[0].pattern_sequence(),
            vec![
                TransitPatternId::new("first"),
                TransitPatternId::new("second")
            ]
        );
    }
}

#[test]
fn consecutive_different_transit_modes_are_allowed() {
    for (first_mode, second_mode) in [
        (TransitMode::Bus, TransitMode::Metro),
        (TransitMode::Metro, TransitMode::Rail),
    ] {
        let network = network(
            vec![
                pattern("first", first_mode, &["o", "hub"]),
                pattern("second", second_mode, &["hub", "d"]),
            ],
            vec![
                trip("first-trip", "first", &[0, 100]),
                trip("second-trip", "second", &[0, 100]),
            ],
        );

        let corridors = run(
            &network,
            &query("o", "d", &[first_mode, second_mode], 1, 10),
        );

        assert_eq!(corridors.retained().len(), 1);
        assert_eq!(
            corridors.retained()[0].pattern_sequence(),
            vec![
                TransitPatternId::new("first"),
                TransitPatternId::new("second")
            ]
        );
    }
}

#[test]
fn arrivals_on_different_routes_keep_their_legal_continuations() {
    let first = pattern("red-a", TransitMode::Metro, &["o", "hub"]);
    let mut last = pattern("red-b", TransitMode::Metro, &["hub", "d"]);
    last.route_id = first.route_id.clone();
    let network = network(
        vec![
            first,
            pattern("blue", TransitMode::Metro, &["o", "hub"]),
            last,
        ],
        vec![
            trip("red-trip", "red-a", &[0, 100]),
            trip("blue-trip", "blue", &[0, 200]),
            trip("last-trip", "red-b", &[0, 100]),
        ],
    );

    let corridors = run(&network, &query("o", "d", &[TransitMode::Metro], 1, 1));

    assert_eq!(corridors.retained().len(), 1);
    assert_eq!(
        corridors.retained()[0].pattern_sequence(),
        vec![
            TransitPatternId::new("blue"),
            TransitPatternId::new("red-b")
        ]
    );
}

#[test]
fn one_global_k_is_shared_across_bus_and_metro() {
    let network = network(
        vec![
            pattern("bus", TransitMode::Bus, &["o", "d"]),
            pattern("metro", TransitMode::Metro, &["o", "d"]),
        ],
        vec![
            trip("bus-trip", "bus", &[0, 100]),
            trip("metro-trip", "metro", &[0, 200]),
        ],
    );

    let retained_corridors = run(
        &network,
        &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 0, 1),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(
        retained_corridors.retained()[0].pattern_sequence(),
        vec![TransitPatternId::new("bus")]
    );
}

#[test]
fn mixed_bus_to_metro_can_outrank_slower_pure_bus() {
    let network = network(
        vec![
            pattern("connector", TransitMode::Bus, &["o", "hub"]),
            pattern("metro", TransitMode::Metro, &["hub", "d"]),
            pattern("slow-bus", TransitMode::Bus, &["o", "d"]),
        ],
        vec![
            trip("metro-trip", "metro", &[0, 300]),
            trip("slow-trip", "slow-bus", &[0, 1_200]),
        ],
    );

    let retained_corridors = run(
        &network,
        &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 1, 1),
    );

    assert_eq!(
        retained_corridors.retained()[0].pattern_sequence(),
        vec![
            TransitPatternId::new("connector"),
            TransitPatternId::new("metro")
        ]
    );
    assert_eq!(
        retained_corridors.retained()[0].cost_source,
        StaticCostSource::Mixed
    );
}

#[test]
fn k_two_means_two_total_corridors_not_two_per_mode() {
    let network = network(
        vec![
            pattern("bus-a", TransitMode::Bus, &["o", "d"]),
            pattern("bus-b", TransitMode::Bus, &["o", "d"]),
            pattern("metro-a", TransitMode::Metro, &["o", "d"]),
            pattern("metro-b", TransitMode::Metro, &["o", "d"]),
        ],
        vec![
            trip("a", "bus-a", &[0, 100]),
            trip("b", "bus-b", &[0, 110]),
            trip("c", "metro-a", &[0, 120]),
            trip("d", "metro-b", &[0, 130]),
        ],
    );

    let (retained_corridors, stats) = generate(
        &TransitRuntime::new(std::sync::Arc::new(network.clone())),
        &TransitTopologyIndex::new(&network),
        &TransferShortcutIndex::from_network(&network),
        &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 0, 2),
    );

    assert_eq!(retained_corridors.retained().len(), 2);
    assert_eq!(stats.corridor_patterns, 4);
}

#[test]
fn equivalent_patterns_share_one_retained_corridor_and_remain_available_for_timing() {
    let network = network(
        vec![
            pattern("bus-a", TransitMode::Bus, &["o", "d"]),
            pattern("bus-b", TransitMode::Bus, &["o", "d"]),
        ],
        vec![trip("a", "bus-a", &[0, 100]), trip("b", "bus-b", &[0, 110])],
    );
    let topology = TransitTopologyIndex::new(&network);
    let transfers = TransferShortcutIndex::from_network(&network);
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));

    let (retained_corridors, stats) = generate(
        &runtime,
        &topology,
        &transfers,
        &query("o", "d", &[TransitMode::Bus], 0, 1),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(stats.corridor_patterns, 2);
    let timing_patterns = retained_corridors
        .timing_positions()
        .map(|position| position.pattern_id)
        .collect::<HashSet<_>>();
    assert!(timing_patterns.contains(&TransitPatternId::new("bus-a")));
    assert!(timing_patterns.contains(&TransitPatternId::new("bus-b")));
}

#[test]
fn retained_corridor_keeps_equivalent_legs_from_overlapping_patterns() {
    let network = network(
        vec![
            pattern("direct", TransitMode::Bus, &["o", "d"]),
            pattern(
                "continues-past-destination",
                TransitMode::Bus,
                &["o", "d", "x"],
            ),
        ],
        vec![
            trip("direct-trip", "direct", &[0, 100]),
            trip("long-trip", "continues-past-destination", &[0, 110, 220]),
        ],
    );
    let topology = TransitTopologyIndex::new(&network);
    let transfers = TransferShortcutIndex::from_network(&network);
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));

    let (retained_corridors, stats) = generate(
        &runtime,
        &topology,
        &transfers,
        &query("o", "d", &[TransitMode::Bus], 0, 1),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(stats.corridor_patterns, 2);
    let timing_patterns = retained_corridors
        .timing_positions()
        .map(|position| position.pattern_id)
        .collect::<HashSet<_>>();
    assert!(timing_patterns.contains(&TransitPatternId::new("direct")));
    assert!(timing_patterns.contains(&TransitPatternId::new("continues-past-destination")));
}

#[test]
fn competitive_metro_to_bus_corridor_is_retained() {
    let network = network(
        vec![
            pattern("bus-direct-a", TransitMode::Bus, &["o", "a", "d"]),
            pattern("bus-direct-b", TransitMode::Bus, &["o", "b", "d"]),
            pattern("metro", TransitMode::Metro, &["o", "m"]),
            pattern("bus-after-metro", TransitMode::Bus, &["x", "d"]),
        ],
        vec![
            trip("a", "bus-direct-a", &[0, 40, 100]),
            trip("b", "bus-direct-b", &[0, 45, 110]),
            trip("m", "metro", &[0, 30]),
            trip("last", "bus-after-metro", &[0, 45]),
        ],
    )
    .with_transfer_edges(vec![TransferEdge::new(id("m"), id("x"), 30, 100)]);
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));

    let retained_corridors = run(
        &network,
        &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 1, 2),
    );

    let mode_sequences = retained_corridors
        .retained()
        .iter()
        .map(|corridor| {
            corridor
                .transit_legs()
                .filter_map(|leg| runtime.pattern(&leg.pattern_id))
                .map(|pattern| runtime.route_of(pattern).mode)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(mode_sequences.contains(&vec![TransitMode::Bus]));
    assert!(mode_sequences.contains(&vec![TransitMode::Metro, TransitMode::Bus]));
}

#[test]
fn multiple_departures_of_one_structure_consume_one_slot() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["o", "d"])],
        vec![
            trip("morning", "bus", &[0, 100]),
            trip("evening", "bus", &[10_000, 10_100]),
        ],
    );

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 100));

    assert_eq!(retained_corridors.retained().len(), 1);
}

#[test]
fn much_slower_detour_is_excluded_when_k_is_full() {
    let network = network(
        vec![
            pattern("direct", TransitMode::Bus, &["o", "d"]),
            pattern("detour", TransitMode::Bus, &["o", "far", "d"]),
        ],
        vec![
            trip("direct-trip", "direct", &[0, 100]),
            trip("detour-trip", "detour", &[0, 1_800, 3_600]),
        ],
    );

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 1));

    assert_eq!(
        retained_corridors.retained()[0].pattern_sequence(),
        vec![TransitPatternId::new("direct")]
    );
}

#[test]
fn bus_without_exact_schedule_uses_optimistic_geometry() {
    let network = network(
        vec![pattern("realtime-only", TransitMode::Bus, &["o", "d"])],
        Vec::new(),
    );

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 1));

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(
        retained_corridors.retained()[0].cost_source,
        StaticCostSource::OptimisticEstimate
    );
}

#[test]
fn selected_mode_filtering_is_applied_before_retention() {
    let network = network(
        vec![
            pattern("bus", TransitMode::Bus, &["o", "d"]),
            pattern("metro", TransitMode::Metro, &["o", "d"]),
        ],
        vec![
            trip("bus-trip", "bus", &[0, 50]),
            trip("metro-trip", "metro", &[0, 100]),
        ],
    );

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Metro], 0, 10));

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(
        retained_corridors.retained()[0].pattern_sequence(),
        vec![TransitPatternId::new("metro")]
    );
}

#[test]
fn maximum_transfer_count_is_respected() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["o", "hub"]),
            pattern("second", TransitMode::Bus, &["hub", "d"]),
        ],
        vec![
            trip("first-trip", "first", &[0, 100]),
            trip("second-trip", "second", &[0, 100]),
        ],
    );

    let blocked = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 10));
    let allowed = run(&network, &query("o", "d", &[TransitMode::Bus], 1, 10));

    assert!(blocked.is_empty());
    assert_eq!(allowed.retained().len(), 1);
    assert_eq!(allowed.retained()[0].pattern_sequence().len(), 2);
}

#[test]
fn static_walking_transfer_can_connect_access_to_first_boarding() {
    let network = network(
        vec![pattern("metro", TransitMode::Metro, &["board", "d"])],
        vec![trip("metro-trip", "metro", &[0, 100])],
    )
    .with_transfer_edges(vec![TransferEdge::new(id("access"), id("board"), 30, 100)]);
    let mut network = network;
    network.stops.push(TransitStop::new(
        id("access"),
        "access",
        GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    ));

    let retained_corridors = run(
        &network,
        &query("access", "d", &[TransitMode::Metro], 0, 10),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    assert!(matches!(
        retained_corridors.retained()[0].steps.first(),
        Some(StaticCorridorStep::SurfaceTransfer { .. })
    ));
}

#[test]
fn static_walking_transfer_cannot_connect_final_alight_to_egress() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["o", "alight"])],
        vec![trip("bus-trip", "bus", &[0, 100])],
    )
    .with_transfer_edges(vec![TransferEdge::new(id("alight"), id("d"), 30, 100)]);
    let mut network = network;
    network.stops.push(TransitStop::new(
        id("d"),
        "d",
        GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    ));

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Bus], 1, 10));

    assert!(retained_corridors.is_empty());
}

#[test]
fn direct_egress_is_retained_over_a_faster_surface_transfer_to_egress() {
    let network = network(
        vec![
            pattern("metro", TransitMode::Metro, &["o", "alight"]),
            pattern("bus", TransitMode::Bus, &["nearby", "elsewhere"]),
        ],
        vec![
            trip("metro-trip", "metro", &[0, 100]),
            trip("bus-trip", "bus", &[0, 100]),
        ],
    )
    .with_transfer_edges(vec![TransferEdge::new(id("alight"), id("nearby"), 30, 100)]);

    for k in [1, 10] {
        let mut query = query("o", "alight", &[TransitMode::Metro, TransitMode::Bus], 1, k);
        query.destination_egress = vec![
            EgressPoint::new(id("alight"), 32, 100),
            EgressPoint::new(id("nearby"), 0, 0),
        ];

        let retained_corridors = run(&network, &query);

        assert_eq!(retained_corridors.retained().len(), 1);
        let corridor = &retained_corridors.retained()[0];
        assert_eq!(corridor.steps.len(), 1);
        assert!(matches!(
            corridor.steps.last(),
            Some(StaticCorridorStep::Transit(leg)) if leg.alight_stop_id == id("alight")
        ));
    }
}

#[test]
fn static_walking_transfer_can_connect_transit_rides() {
    let network = network(
        vec![
            pattern("metro", TransitMode::Metro, &["o", "alight"]),
            pattern("bus", TransitMode::Bus, &["board", "d"]),
        ],
        vec![
            trip("metro-trip", "metro", &[0, 100]),
            trip("bus-trip", "bus", &[0, 100]),
        ],
    )
    .with_transfer_edges(vec![TransferEdge::new(id("alight"), id("board"), 30, 100)]);

    let retained_corridors = run(
        &network,
        &query("o", "d", &[TransitMode::Metro, TransitMode::Bus], 1, 10),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    let corridor = &retained_corridors.retained()[0];
    assert_eq!(
        corridor.pattern_sequence(),
        vec![TransitPatternId::new("metro"), TransitPatternId::new("bus")]
    );
    assert_eq!(corridor.steps.len(), 3);
    assert!(matches!(
        &corridor.steps[1],
        StaticCorridorStep::SurfaceTransfer { from, to, .. }
            if from == &id("alight") && to == &id("board")
    ));
    assert!(matches!(
        corridor.steps.last(),
        Some(StaticCorridorStep::Transit(leg)) if leg.alight_stop_id == id("d")
    ));
}

#[test]
fn expired_deadline_stops_phase1_before_expanding_a_state() {
    let network = network(
        vec![pattern("bus", TransitMode::Bus, &["o", "d"])],
        vec![trip("bus-trip", "bus", &[0, 100])],
    );
    let topology = TransitTopologyIndex::new(&network);
    let transfers = TransferShortcutIndex::from_network(&network);
    let mut query = query("o", "d", &[TransitMode::Bus], 0, 2);
    query.deadline = super::super::model::PlanningDeadline::elapsed_for_test();
    let runtime = TransitRuntime::new(std::sync::Arc::new(network));

    let (corridors, stats) = generate(&runtime, &topology, &transfers, &query);

    assert!(stats.deadline_exceeded);
    assert_eq!(stats.states_expanded, 0);
    assert!(corridors.is_empty());
}

#[test]
fn best_corridor_for_each_ride_count_survives_a_full_cap_of_faster_transfers() {
    let mut patterns = vec![pattern("direct-rail", TransitMode::Rail, &["o", "d"])];
    let mut trips = vec![trip("direct-trip", "direct-rail", &[0, 2_400])];
    for index in 0..4 {
        let hub = format!("hub-{index}");
        let first = format!("feeder-{index}");
        let second = format!("express-{index}");
        patterns.push(pattern(&first, TransitMode::Bus, &["o", &hub]));
        patterns.push(pattern(&second, TransitMode::Bus, &[&hub, "d"]));
        trips.push(trip(&format!("{first}-trip"), &first, &[0, 300]));
        trips.push(trip(&format!("{second}-trip"), &second, &[0, 900]));
    }
    let network = network(patterns, trips);

    let retained_corridors = run(
        &network,
        &query("o", "d", &[TransitMode::Bus, TransitMode::Rail], 1, 2),
    );

    assert_eq!(retained_corridors.retained().len(), 2);
    assert!(
        retained_corridors
            .retained()
            .iter()
            .any(|corridor| corridor.pattern_sequence()
                == vec![TransitPatternId::new("direct-rail")])
    );
}

#[test]
fn transfer_cost_prefers_a_direct_ride_over_a_slightly_faster_transfer() {
    let network = network(
        vec![
            pattern("direct", TransitMode::Bus, &["o", "d"]),
            pattern("first", TransitMode::Bus, &["o", "hub"]),
            pattern("second", TransitMode::Bus, &["hub", "d"]),
        ],
        vec![
            trip("direct-trip", "direct", &[0, 500]),
            trip("first-trip", "first", &[0, 200]),
            trip("second-trip", "second", &[0, 200]),
        ],
    );

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Bus], 1, 1));

    assert_eq!(
        retained_corridors
            .retained()
            .iter()
            .map(StaticCorridor::pattern_sequence)
            .collect::<Vec<_>>(),
        vec![vec![TransitPatternId::new("direct")]]
    );
}

#[test]
fn target_bound_drops_much_slower_corridors_when_the_cap_is_not_full() {
    let network = network(
        vec![
            pattern("direct", TransitMode::Bus, &["o", "d"]),
            pattern("first", TransitMode::Bus, &["o", "hub"]),
            pattern("second", TransitMode::Bus, &["hub", "d"]),
        ],
        vec![
            trip("direct-trip", "direct", &[0, 600]),
            trip("first-trip", "first", &[0, 1_200]),
            trip("second-trip", "second", &[0, 1_200]),
        ],
    );

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Bus], 1, 24));

    assert_eq!(
        retained_corridors
            .retained()
            .iter()
            .map(StaticCorridor::pattern_sequence)
            .collect::<Vec<_>>(),
        vec![vec![TransitPatternId::new("direct")]]
    );
}

#[test]
fn equal_cost_walking_transfers_are_kept_in_stop_order() {
    let mut patterns = vec![pattern("rail", TransitMode::Rail, &["x", "d"])];
    let mut trips = vec![trip("rail-trip", "rail", &[0, 600])];
    let mut edges = Vec::new();
    for index in 0..8 {
        let hub = format!("hub-{index}");
        let feeder = format!("feeder-{index}");
        patterns.push(pattern(&feeder, TransitMode::Bus, &["o", &hub]));
        trips.push(trip(&format!("{feeder}-trip"), &feeder, &[0, 300]));
        edges.push(TransferEdge::new(id(&hub), id("x"), 60, 80));
    }
    let network = network(patterns, trips).with_transfer_edges(edges);
    let query = query("o", "d", &[TransitMode::Bus, TransitMode::Rail], 1, 24);
    let feeders = || {
        run(&network, &query)
            .retained()
            .iter()
            .map(|corridor| corridor.pattern_sequence()[0].as_str().to_owned())
            .collect::<Vec<_>>()
    };

    let first = feeders();

    assert_eq!(first, ["feeder-0", "feeder-1", "feeder-2", "feeder-3"]);
    for _ in 0..20 {
        assert_eq!(feeders(), first);
    }
}

#[test]
fn rail_patterns_sharing_one_route_can_be_connected() {
    for mode in [TransitMode::Rail, TransitMode::HighSpeedRail] {
        let first = pattern("main-line", mode, &["o", "hub"]);
        let mut second = pattern("branch-line", mode, &["hub", "d"]);
        second.route_id = first.route_id.clone();
        let network = network(
            vec![first, second],
            vec![
                trip("main-trip", "main-line", &[0, 100]),
                trip("branch-trip", "branch-line", &[0, 100]),
            ],
        );

        let corridors = run(&network, &query("o", "d", &[mode], 1, 10));

        assert_eq!(corridors.retained().len(), 1, "{mode:?}");
        assert_eq!(
            corridors.retained()[0].pattern_sequence(),
            vec![
                TransitPatternId::new("main-line"),
                TransitPatternId::new("branch-line")
            ]
        );
    }
}

#[test]
fn retained_corridors_prefer_distinct_ride_sequences_over_stop_variants() {
    let network = network(
        vec![
            pattern("main", TransitMode::Bus, &["o1", "o2", "o3", "d"]),
            pattern("other", TransitMode::Bus, &["x", "d"]),
        ],
        vec![
            trip("main-trip", "main", &[0, 60, 120, 600]),
            trip("other-trip", "other", &[0, 900]),
        ],
    );
    let mut query = query("o1", "d", &[TransitMode::Bus], 0, 2);
    query.origin_access = ["o1", "o2", "o3", "x"]
        .into_iter()
        .enumerate()
        .map(|(index, stop)| {
            AccessPoint::new(
                id(stop),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                60 * index as u32,
                0,
            )
        })
        .collect();

    let corridors = run(&network, &query);

    let sequences = corridors
        .retained()
        .iter()
        .map(|corridor| corridor.pattern_sequence())
        .collect::<HashSet<_>>();
    assert_eq!(corridors.retained().len(), 2);
    assert!(sequences.contains(&vec![TransitPatternId::new("other")]));
}
