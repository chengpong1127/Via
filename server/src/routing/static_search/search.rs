use std::{collections::HashMap, time::Instant};

use crate::domain::{ids::BoardingPointId, transit_runtime::TransitRuntime};

use crate::routing::{
    model::PlanningQuery,
    topology::{PatternPosition, TransitTopologyIndex},
    transfers::TransferShortcutIndex,
};

use super::{
    frontier::{SearchFrontier, StaticState},
    heuristic::destination_lower_bounds,
    pattern_index::StaticPhase1PatternIndex,
    retention::{CorridorRetention, RetainedCorridorSet},
    types::{
        EquivalentLegKey, StaticCorridorStep, StaticCostSource, StaticSearchStats, StaticTransitLeg,
    },
};

struct PreparedSearch<'a> {
    network: &'a TransitRuntime,
    topology: &'a TransitTopologyIndex,
    transfers: &'a TransferShortcutIndex,
    query: &'a PlanningQuery,
    patterns: StaticPhase1PatternIndex,
    egress_by_stop: HashMap<BoardingPointId, Vec<u32>>,
    destination_lower_bounds: HashMap<BoardingPointId, u32>,
    max_legs: usize,
}

pub fn generate(
    network: &TransitRuntime,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &PlanningQuery,
) -> (RetainedCorridorSet, StaticSearchStats) {
    let started = Instant::now();
    let limit = query.limits.max_static_corridors.max(1);
    let mut stats = StaticSearchStats::default();
    let Some(mut search) = prepare_search(network, topology, transfers, query, &mut stats) else {
        return finish_deadline(started, stats);
    };
    let mut frontier = seed_frontier(&search, limit);
    let mut retention = CorridorRetention::new(limit);
    let mut equivalent_legs = HashMap::<EquivalentLegKey, Vec<StaticTransitLeg>>::new();
    run_forward_search(
        &mut search,
        &mut frontier,
        &mut retention,
        &mut equivalent_legs,
        &mut stats,
    );

    if stats.deadline_exceeded || query.deadline.is_elapsed() {
        return finish_deadline(started, stats);
    }

    let retained_corridors = retention.finish(network, &equivalent_legs, &mut stats);
    stats.search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    (retained_corridors, stats)
}

fn prepare_search<'a>(
    network: &'a TransitRuntime,
    topology: &'a TransitTopologyIndex,
    transfers: &'a TransferShortcutIndex,
    query: &'a PlanningQuery,
    stats: &mut StaticSearchStats,
) -> Option<PreparedSearch<'a>> {
    if deadline_exceeded(query) {
        stats.deadline_exceeded = true;
        return None;
    }

    let patterns =
        StaticPhase1PatternIndex::new(network, &query.allowed_transit_modes, stats, query.deadline);
    if stats.deadline_exceeded || deadline_exceeded(query) {
        stats.deadline_exceeded = true;
        return None;
    }

    let egress_by_stop = build_egress_index(query);
    let destination_lower_bounds =
        destination_lower_bounds(&patterns, topology, transfers, query, stats);

    if stats.deadline_exceeded || deadline_exceeded(query) {
        stats.deadline_exceeded = true;
        return None;
    }

    Some(PreparedSearch {
        network,
        topology,
        transfers,
        query,
        patterns,
        egress_by_stop,
        destination_lower_bounds,
        max_legs: query.limits.max_transfers.saturating_add(1),
    })
}

fn build_egress_index(query: &PlanningQuery) -> HashMap<BoardingPointId, Vec<u32>> {
    query.destination_egress.iter().fold(
        HashMap::<BoardingPointId, Vec<u32>>::new(),
        |mut by_stop, egress| {
            by_stop
                .entry(egress.stop_id.clone())
                .or_default()
                .push(egress.walking_seconds);
            by_stop
        },
    )
}

fn seed_frontier(search: &PreparedSearch<'_>, limit: usize) -> SearchFrontier {
    let mut frontier = SearchFrontier::new(limit);
    for access in &search.query.origin_access {
        let Some(remaining_seconds) = search
            .destination_lower_bounds
            .get(&access.stop_id)
            .copied()
        else {
            continue;
        };
        let elapsed_seconds = access.walking_seconds;
        frontier.enqueue(StaticState {
            stop_id: access.stop_id.clone(),
            elapsed_seconds,
            lower_bound_seconds: elapsed_seconds.saturating_add(remaining_seconds),
            priority_seconds: elapsed_seconds.saturating_add(remaining_seconds),
            access_stop_id: access.stop_id.clone(),
            transit_legs_used: 0,
            last_transit_mode: None,
            mode_transition_sequence: Vec::new(),
            steps: Vec::new(),
            has_exact_cost: false,
            has_derived_bus_cost: false,
            has_estimated_cost: false,
            transfer_ready: false,
        });
    }
    frontier
}

fn run_forward_search(
    search: &mut PreparedSearch<'_>,
    frontier: &mut SearchFrontier,
    retention: &mut CorridorRetention,
    equivalent_legs: &mut HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    stats: &mut StaticSearchStats,
) {
    while let Some(entry) = frontier.pop() {
        if deadline_exceeded(search.query) {
            stats.deadline_exceeded = true;
            break;
        }
        if retention
            .kth_duration()
            .is_some_and(|kth| entry.state.priority_seconds > kth)
        {
            stats.top_k_early_terminated = true;
            break;
        }
        if !frontier.is_current(&entry.state) {
            continue;
        }
        if stats.states_expanded >= search.query.limits.max_phase1_expansions {
            stats.search_limit_reached = true;
            break;
        }
        stats.states_expanded += 1;
        let state = entry.state;

        if !state.transfer_ready
            && expand_surface_transfers(&state, search, frontier, retention, stats)
        {
            break;
        }

        if state.transit_legs_used >= search.max_legs {
            continue;
        }

        if expand_transit_rides(&state, search, frontier, retention, equivalent_legs, stats) {
            break;
        }
    }
}

fn usable_pattern_occurrences(
    state: &StaticState,
    search: &PreparedSearch<'_>,
) -> Vec<PatternPosition> {
    search
        .topology
        .occurrences_at(&state.stop_id)
        .iter()
        .filter(|position| search.patterns.group_index(&position.pattern_id).is_some())
        .cloned()
        .collect()
}

fn expand_surface_transfers(
    state: &StaticState,
    search: &PreparedSearch<'_>,
    frontier: &mut SearchFrontier,
    retention: &mut CorridorRetention,
    stats: &mut StaticSearchStats,
) -> bool {
    for shortcut in search.transfers.transfers_from(&state.stop_id) {
        if deadline_exceeded(search.query) {
            stats.deadline_exceeded = true;
            return true;
        }

        let elapsed_seconds = state
            .elapsed_seconds
            .saturating_add(shortcut.duration_seconds);
        if elapsed_seconds > search.query.limits.max_journey_duration_seconds {
            continue;
        }
        let mut next = state.clone();
        next.stop_id = shortcut.to.clone();
        next.elapsed_seconds = elapsed_seconds;
        let Some(remaining_seconds) = search.destination_lower_bounds.get(&shortcut.to).copied()
        else {
            continue;
        };
        next.lower_bound_seconds = elapsed_seconds.saturating_add(remaining_seconds);
        next.priority_seconds = next.lower_bound_seconds;
        next.steps.push(StaticCorridorStep::SurfaceTransfer {
            from: shortcut.from.clone(),
            to: shortcut.to.clone(),
            walking_duration_seconds: shortcut.duration_seconds,
            walking_distance_metres: shortcut.distance_metres,
        });
        next.transfer_ready = true;
        if next.transit_legs_used > 0 {
            retention.record_complete(
                &next,
                &search.egress_by_stop,
                search.query.limits.max_journey_duration_seconds,
                stats,
                search.network,
            );
        }
        if next.transit_legs_used < search.max_legs {
            frontier.enqueue(next);
        }
    }
    false
}

fn expand_transit_rides(
    state: &StaticState,
    search: &mut PreparedSearch<'_>,
    frontier: &mut SearchFrontier,
    retention: &mut CorridorRetention,
    equivalent_legs: &mut HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    stats: &mut StaticSearchStats,
) -> bool {
    for position in usable_pattern_occurrences(state, search) {
        if deadline_exceeded(search.query) {
            stats.deadline_exceeded = true;
            return true;
        }
        let group_index = search
            .patterns
            .group_index(&position.pattern_id)
            .expect("indexed pattern");
        let group = search.patterns.group(group_index);
        let mode = group.mode;
        if !search.query.allowed_transit_modes.contains(&mode) {
            continue;
        }

        search.patterns.ensure_ride_options(
            group_index,
            position.stop_index,
            stats,
            equivalent_legs,
            search.query.deadline,
        );
        if stats.deadline_exceeded {
            return true;
        }
        let transfer_seconds = if state.transit_legs_used > 0 && !state.transfer_ready {
            search.network.same_stop_transfer_seconds()
        } else {
            0
        };
        for ride_option in search
            .patterns
            .ride_options(group_index, position.stop_index)
        {
            if deadline_exceeded(search.query) {
                stats.deadline_exceeded = true;
                return true;
            }

            let ride_seconds = ride_option.duration_seconds;
            let source = ride_option.leg.cost_source;
            let elapsed_seconds = state
                .elapsed_seconds
                .saturating_add(transfer_seconds)
                .saturating_add(ride_seconds);
            if elapsed_seconds > search.query.limits.max_journey_duration_seconds {
                continue;
            }
            let alight_stop_id = ride_option.alight_stop_id.clone();
            let mut next = state.clone();
            next.stop_id = alight_stop_id.clone();
            next.elapsed_seconds = elapsed_seconds;
            let Some(remaining_seconds) = search
                .destination_lower_bounds
                .get(&alight_stop_id)
                .copied()
            else {
                continue;
            };
            next.lower_bound_seconds = elapsed_seconds.saturating_add(remaining_seconds);
            next.priority_seconds = next.lower_bound_seconds;
            next.transit_legs_used += 1;
            if state.last_transit_mode != Some(mode) {
                next.mode_transition_sequence.push(mode);
            }
            next.last_transit_mode = Some(mode);
            next.steps
                .push(StaticCorridorStep::Transit(ride_option.leg.clone()));
            next.has_exact_cost |= source == StaticCostSource::ExactSchedule;
            next.has_derived_bus_cost |= source == StaticCostSource::DerivedBus;
            next.has_estimated_cost |= source == StaticCostSource::OptimisticEstimate;
            if source == StaticCostSource::Mixed {
                next.has_exact_cost = true;
                next.has_estimated_cost = true;
            }
            next.transfer_ready = false;
            retention.record_complete(
                &next,
                &search.egress_by_stop,
                search.query.limits.max_journey_duration_seconds,
                stats,
                search.network,
            );
            frontier.enqueue(next);
        }
    }
    false
}

fn deadline_exceeded(query: &PlanningQuery) -> bool {
    query.deadline.is_elapsed()
}

fn finish_deadline(
    started: Instant,
    mut stats: StaticSearchStats,
) -> (RetainedCorridorSet, StaticSearchStats) {
    stats.deadline_exceeded = true;
    stats.search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    (RetainedCorridorSet::default(), stats)
}
