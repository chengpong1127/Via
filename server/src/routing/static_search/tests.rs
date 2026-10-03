use std::collections::{HashMap, HashSet};

use time::{Time, Weekday, macros::datetime};

use super::{
    heuristic::{EndpointModePair, reachable_endpoint_mode_pairs},
    *,
};
use crate::{
    api::GeoCoordinate,
    domain::{
        ids::{BoardingPointId, TransitPatternId, TransitTripId},
        transit::{
            ScheduledStopTime, TransferEdge, TransitMode, TransitNetwork, TransitPattern,
            TransitStop, TransitTrip,
        },
        transit_runtime::TransitRuntime,
    },
    routing::{
        AccessPoint, EgressPoint, model::PlanningQuery, topology::TransitTopologyIndex,
        transfers::TransferShortcutIndex,
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
    .with_max_static_corridors(k)
}

fn run(network: &TransitNetwork, query: &PlanningQuery) -> RetainedCorridorSet {
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));
    let topology = TransitTopologyIndex::new(runtime.network());
    let transfers = TransferShortcutIndex::from_network(runtime.network());
    generate(&runtime, &topology, &transfers, query).0
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
fn top_k_does_not_require_every_endpoint_mode_pair() {
    let network = network(
        vec![
            pattern("bus", TransitMode::Bus, &["o", "d"]),
            pattern("metro", TransitMode::Metro, &["o", "d"]),
            pattern("slow-first", TransitMode::Bus, &["o", "slow-hub"]),
            pattern("slow-last", TransitMode::Bus, &["slow-hub", "d"]),
        ],
        vec![
            trip("bus-trip", "bus", &[0, 100]),
            trip("metro-trip", "metro", &[0, 200]),
            trip("slow-first-trip", "slow-first", &[0, 500]),
            trip("slow-last-trip", "slow-last", &[0, 500]),
        ],
    );
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));

    let (retained_corridors, stats) = generate(
        &runtime,
        &TransitTopologyIndex::new(&network),
        &TransferShortcutIndex::from_network(&network),
        &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 1, 1),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(
        retained_corridors.retained()[0].pattern_sequence(),
        vec![TransitPatternId::new("bus")]
    );
    assert_eq!(stats.static_required_endpoint_mode_pairs.len(), 4);
    assert_eq!(stats.static_complete_endpoint_mode_pairs.len(), 2);
    assert_eq!(stats.static_search_states_expanded, 2);
    assert!(stats.static_top_k_early_terminated);
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
    assert_eq!(stats.static_corridor_patterns, 4);
}

#[test]
fn top_k_search_expands_equal_cost_bounds_before_terminating() {
    let network = network(
        vec![
            pattern("direct", TransitMode::Bus, &["o", "d"]),
            pattern("first", TransitMode::Bus, &["o", "hub-from"]),
            pattern("second", TransitMode::Bus, &["hub-to", "d"]),
        ],
        vec![
            trip("direct-trip", "direct", &[0, 3]),
            trip("first-trip", "first", &[0, 1]),
            trip("second-trip", "second", &[0, 1]),
        ],
    )
    .with_transfer_edges(vec![TransferEdge::new(
        id("hub-from"),
        id("hub-to"),
        1,
        100,
    )]);

    let (retained_corridors, stats) = generate(
        &TransitRuntime::new(std::sync::Arc::new(network.clone())),
        &TransitTopologyIndex::new(&network),
        &TransferShortcutIndex::from_network(&network),
        &query("o", "d", &[TransitMode::Bus], 1, 1),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(stats.static_unique_corridors_found, 2);
}

#[test]
fn top_k_stops_when_best_frontier_bound_exceeds_kth_duration() {
    let mut patterns = vec![pattern("direct", TransitMode::Bus, &["o", "d"])];
    let mut trips = vec![trip("direct-trip", "direct", &[0, 3])];
    for index in 0..64 {
        let first_id = format!("first-{index}");
        let second_id = format!("second-{index}");
        let feeder_stop = format!("feeder-{index}");
        patterns.push(pattern(&first_id, TransitMode::Bus, &["o", &feeder_stop]));
        patterns.push(pattern(&second_id, TransitMode::Bus, &[&feeder_stop, "d"]));
        trips.push(trip(&format!("{first_id}-trip"), &first_id, &[0, 10]));
        trips.push(trip(&format!("{second_id}-trip"), &second_id, &[0, 10]));
    }
    let network = network(patterns, trips);
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));

    let (retained_corridors, stats) = generate(
        &runtime,
        &TransitTopologyIndex::new(&network),
        &TransferShortcutIndex::from_network(&network),
        &query("o", "d", &[TransitMode::Bus], 1, 1),
    );

    assert_eq!(retained_corridors.retained().len(), 1);
    assert_eq!(stats.kth_static_duration_seconds, Some(3));
    assert_eq!(stats.static_search_states_expanded, 2);
    assert!(stats.static_top_k_early_terminated);
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
    assert_eq!(stats.static_corridor_patterns, 2);
    let timing_patterns = retained_corridors
        .timing_positions()
        .map(|position| position.pattern_id)
        .collect::<HashSet<_>>();
    assert!(timing_patterns.contains(&TransitPatternId::new("bus-a")));
    assert!(timing_patterns.contains(&TransitPatternId::new("bus-b")));
    assert_eq!(
        stats.static_mode_stats[&TransitMode::Bus].distinct_pattern_ids,
        2
    );
    assert_eq!(
        stats.static_mode_stats[&TransitMode::Bus].distinct_stop_sequences,
        1
    );
    assert!(stats.static_structurally_equivalent_states_rejected > 0);
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
    assert_eq!(stats.static_corridor_patterns, 2);
    let timing_patterns = retained_corridors
        .timing_positions()
        .map(|position| position.pattern_id)
        .collect::<HashSet<_>>();
    assert!(timing_patterns.contains(&TransitPatternId::new("direct")));
    assert!(timing_patterns.contains(&TransitPatternId::new("continues-past-destination")));
}

#[test]
fn competitive_metro_to_bus_corridor_is_found_in_global_top_k() {
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
fn static_walking_transfer_can_connect_final_alight_to_egress() {
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

    let retained_corridors = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 10));

    assert_eq!(retained_corridors.retained().len(), 1);
    assert!(matches!(
        retained_corridors.retained()[0].steps.last(),
        Some(StaticCorridorStep::SurfaceTransfer { from, to, .. })
            if from == &id("alight") && to == &id("d")
    ));
}

#[test]
fn expansion_bound_is_reported_instead_of_silently_certifying_partial_top_k() {
    let network = network(
        vec![
            pattern("first", TransitMode::Bus, &["o", "d"]),
            pattern("second", TransitMode::Bus, &["o", "d"]),
        ],
        vec![
            trip("first-trip", "first", &[0, 100]),
            trip("second-trip", "second", &[0, 110]),
        ],
    );
    let topology = TransitTopologyIndex::new(&network);
    let transfers = TransferShortcutIndex::from_network(&network);
    let mut query = query("o", "d", &[TransitMode::Bus], 0, 2);
    query.limits.max_phase1_expansions = 1;
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));

    let (_, stats) = generate(&runtime, &topology, &transfers, &query);

    assert!(stats.static_search_limit_reached);
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
    assert_eq!(stats.static_search_states_expanded, 0);
    assert!(corridors.is_empty());
}
#[test]
fn endpoint_mode_discovery_cancels_expired_queries_without_partial_results() {
    let network = network(
        (0..256)
            .map(|index| pattern(&format!("bus-{index}"), TransitMode::Bus, &["o", "d"]))
            .collect(),
        Vec::new(),
    );
    let topology = TransitTopologyIndex::new(&network);
    let transfers = TransferShortcutIndex::from_network(&network);
    let runtime = TransitRuntime::new(std::sync::Arc::new(network));
    let mut query = query("o", "d", &[TransitMode::Bus], 0, 2);
    let lower_bounds = HashMap::from([(id("o"), 100), (id("d"), 0)]);
    assert_eq!(
        reachable_endpoint_mode_pairs(&runtime, &topology, &transfers, &query, &lower_bounds),
        Some(HashSet::from([EndpointModePair {
            first: TransitMode::Bus,
            last: TransitMode::Bus
        }])),
    );
    query.deadline = super::super::model::PlanningDeadline::elapsed_for_test();
    assert_eq!(
        reachable_endpoint_mode_pairs(&runtime, &topology, &transfers, &query, &lower_bounds),
        None
    );
}

#[test]
fn pattern_ride_cache_keeps_one_ride_per_position_pair() {
    use super::pattern_index::StaticPhase1PatternIndex;

    let network = network(
        vec![
            pattern("loop-a", TransitMode::Bus, &["o", "mid", "o", "d"]),
            pattern("loop-b", TransitMode::Bus, &["o", "mid", "o", "d"]),
        ],
        vec![
            trip("a", "loop-a", &[0, 10, 20, 30]),
            trip("b", "loop-b", &[0, 15, 30, 45]),
        ],
    );
    let runtime = TransitRuntime::new(std::sync::Arc::new(network));
    let mut stats = StaticSearchStats::default();
    let mut index = StaticPhase1PatternIndex::new(
        &runtime,
        &[TransitMode::Bus],
        &mut stats,
        crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
    );
    let mut equivalent_legs = HashMap::new();
    for (pattern_id, segment_seconds) in [("loop-a", 10), ("loop-b", 15)] {
        let group_index = index
            .group_index(&TransitPatternId::new(pattern_id))
            .unwrap();
        for board_index in 0..4 {
            assert!(index.ensure_ride_options(
                group_index,
                board_index,
                &mut stats,
                &mut equivalent_legs,
                crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(
                    3600
                ))
            ));
            let rides = index.ride_options(group_index, board_index);
            assert_eq!(rides.len(), 3 - board_index);
            for (offset, ride) in rides.iter().enumerate() {
                let alight_index = board_index + offset + 1;
                assert_eq!(ride.leg.pattern_id.as_str(), pattern_id);
                assert_eq!(ride.leg.board_index, board_index);
                assert_eq!(ride.leg.alight_index, alight_index);
                assert_eq!(
                    ride.duration_seconds,
                    (alight_index - board_index) as u32 * segment_seconds
                );
            }
            assert!(!index.ensure_ride_options(
                group_index,
                board_index,
                &mut stats,
                &mut equivalent_legs,
                crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(
                    3600
                ))
            ));
        }
    }
    let legs = equivalent_legs
        .values()
        .find(|legs| legs.len() == 4)
        .unwrap();
    assert!(
        legs.iter()
            .all(|leg| leg.board_stop_id == id("o") && leg.alight_stop_id == id("d"))
    );
}
