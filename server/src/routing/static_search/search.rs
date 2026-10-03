use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Instant,
};

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::TransitMode,
    transit_runtime::TransitRuntime,
};

use crate::routing::{
    model::PlanningQuery,
    topology::{PatternPosition, TransitTopologyIndex},
    transfers::TransferShortcutIndex,
};

use super::{
    frontier::{SearchFrontier, StaticState},
    heuristic::{destination_lower_bounds, reachable_endpoint_mode_pairs, sorted_mode_pairs},
    pattern_index::StaticPhase1PatternIndex,
    retention::{CorridorRetention, RetainedCorridorSet},
    types::{
        EquivalentLegKey, StaticCorridorStep, StaticCostSource, StaticSearchStats,
        StaticTransitLeg, TransitModeSearchStats,
    },
};

#[derive(Default)]
struct ModeSearchAccumulator {
    states_expanded: usize,
    pattern_scans: usize,
    downstream_positions_scanned: usize,
    pattern_ids: HashSet<TransitPatternId>,
    stop_sequences: HashSet<Vec<BoardingPointId>>,
}

#[derive(Default)]
struct SearchExpansionStats {
    states_by_leg_count: HashMap<usize, usize>,
    expanded_stops: HashMap<BoardingPointId, usize>,
    modes: HashMap<TransitMode, ModeSearchAccumulator>,
}

struct SearchAccounting<'a> {
    stats: &'a mut StaticSearchStats,
    expansion_stats: &'a mut SearchExpansionStats,
}

struct PreparedSearch<'a> {
    network: &'a TransitRuntime,
    topology: &'a TransitTopologyIndex,
    transfers: &'a TransferShortcutIndex,
    query: &'a PlanningQuery,
    patterns: StaticPhase1PatternIndex,
    egress_by_stop: HashMap<BoardingPointId, Vec<u32>>,
    terminal_transfer_source_stops: HashSet<BoardingPointId>,
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
    let mut frontier = seed_frontier(&search, limit, &mut stats);
    let forward_search_started = Instant::now();
    let mut retention = CorridorRetention::new(limit);
    let mut equivalent_legs = HashMap::<EquivalentLegKey, Vec<StaticTransitLeg>>::new();
    let mut expansion_stats = SearchExpansionStats::default();
    run_forward_search(
        &mut search,
        &mut frontier,
        &mut retention,
        &mut equivalent_legs,
        &mut stats,
        &mut expansion_stats,
    );

    if stats.deadline_exceeded || query.deadline.is_elapsed() {
        stats.static_forward_search_latency_us = elapsed_microseconds(forward_search_started);
        return finish_deadline(started, stats);
    }

    stats.static_forward_search_latency_us = elapsed_microseconds(forward_search_started);
    finalize_expansion_stats(&mut stats, expansion_stats);

    let retention_started = Instant::now();
    let retained_corridors = retention.finish(network, &equivalent_legs, &mut stats);
    stats.static_retention_latency_us = elapsed_microseconds(retention_started);
    stats.static_search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
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

    let pattern_index_started = Instant::now();
    let patterns =
        StaticPhase1PatternIndex::new(network, &query.allowed_transit_modes, stats, query.deadline);
    stats.static_pattern_index_build_latency_us = elapsed_microseconds(pattern_index_started);
    if stats.deadline_exceeded || deadline_exceeded(query) {
        stats.deadline_exceeded = true;
        return None;
    }

    let egress_by_stop = build_egress_index(query);
    let terminal_transfer_source_stops =
        collect_terminal_transfer_sources(&egress_by_stop, transfers);

    let lower_bound_started = Instant::now();
    let destination_lower_bounds =
        destination_lower_bounds(&patterns, topology, transfers, query, stats);
    stats.static_lower_bound_latency_us = elapsed_microseconds(lower_bound_started);
    if stats.deadline_exceeded || deadline_exceeded(query) {
        stats.deadline_exceeded = true;
        return None;
    }

    let Some(required_mode_pairs) = reachable_endpoint_mode_pairs(
        network,
        topology,
        transfers,
        query,
        &destination_lower_bounds,
    ) else {
        stats.deadline_exceeded = true;
        return None;
    };
    if deadline_exceeded(query) {
        stats.deadline_exceeded = true;
        return None;
    }
    stats.static_required_endpoint_mode_pairs = sorted_mode_pairs(&required_mode_pairs);

    Some(PreparedSearch {
        network,
        topology,
        transfers,
        query,
        patterns,
        egress_by_stop,
        terminal_transfer_source_stops,
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

fn collect_terminal_transfer_sources(
    egress_by_stop: &HashMap<BoardingPointId, Vec<u32>>,
    transfers: &TransferShortcutIndex,
) -> HashSet<BoardingPointId> {
    egress_by_stop
        .keys()
        .flat_map(|egress_stop| transfers.transfers_to(egress_stop))
        .map(|shortcut| shortcut.from.clone())
        .collect()
}

fn seed_frontier(
    search: &PreparedSearch<'_>,
    limit: usize,
    stats: &mut StaticSearchStats,
) -> SearchFrontier {
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
        frontier.enqueue(
            StaticState {
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
            },
            stats,
        );
    }
    frontier
}

fn run_forward_search(
    search: &mut PreparedSearch<'_>,
    frontier: &mut SearchFrontier,
    retention: &mut CorridorRetention,
    equivalent_legs: &mut HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    stats: &mut StaticSearchStats,
    expansion_stats: &mut SearchExpansionStats,
) {
    let mut accounting = SearchAccounting {
        stats,
        expansion_stats,
    };
    while let Some(entry) = frontier.pop() {
        if deadline_exceeded(search.query) {
            accounting.stats.deadline_exceeded = true;
            break;
        }
        if retention
            .kth_duration()
            .is_some_and(|kth| entry.state.priority_seconds > kth)
        {
            accounting.stats.static_top_k_early_terminated = true;
            break;
        }
        if !frontier.is_current(&entry.state) {
            continue;
        }
        if accounting.stats.static_search_states_expanded
            >= search.query.limits.max_phase1_expansions
        {
            accounting.stats.static_search_limit_reached = true;
            break;
        }
        accounting.stats.static_search_states_expanded += 1;
        let state = entry.state;
        let pattern_occurrences = record_expanded_state_stats(&state, search, &mut accounting);

        if !state.transfer_ready
            && expand_surface_transfers(&state, search, frontier, retention, accounting.stats)
        {
            break;
        }

        if state.transit_legs_used >= search.max_legs {
            continue;
        }

        if expand_transit_rides(
            &state,
            &pattern_occurrences,
            search,
            frontier,
            retention,
            equivalent_legs,
            &mut accounting,
        ) {
            break;
        }
    }
}

fn record_expanded_state_stats(
    state: &StaticState,
    search: &PreparedSearch<'_>,
    accounting: &mut SearchAccounting<'_>,
) -> Vec<PatternPosition> {
    *accounting
        .expansion_stats
        .states_by_leg_count
        .entry(state.transit_legs_used)
        .or_default() += 1;
    *accounting
        .expansion_stats
        .expanded_stops
        .entry(state.stop_id.clone())
        .or_default() += 1;
    if search.egress_by_stop.contains_key(&state.stop_id) {
        accounting.stats.static_expanded_egress_states += 1;
    }
    if search
        .terminal_transfer_source_stops
        .contains(&state.stop_id)
    {
        accounting
            .stats
            .static_expanded_terminal_transfer_source_states += 1;
    }
    if let Some(mode) = state.last_transit_mode {
        accounting
            .expansion_stats
            .modes
            .entry(mode)
            .or_default()
            .states_expanded += 1;
    } else {
        accounting.stats.static_unattributed_states_expanded += 1;
    }
    let occurrences = search.topology.occurrences_at(&state.stop_id);
    let pattern_occurrences = occurrences
        .iter()
        .filter(|position| search.patterns.group_index(&position.pattern_id).is_some())
        .cloned()
        .collect::<Vec<_>>();
    accounting.stats.static_expanded_stop_occurrence_samples += 1;
    accounting.stats.static_expanded_stop_occurrences_total += occurrences.len();
    accounting
        .stats
        .static_expanded_stop_pattern_occurrences_total += pattern_occurrences.len();
    accounting.stats.static_expanded_stop_occurrences_max = accounting
        .stats
        .static_expanded_stop_occurrences_max
        .max(occurrences.len());
    accounting
        .stats
        .static_expanded_stop_pattern_occurrences_max = accounting
        .stats
        .static_expanded_stop_pattern_occurrences_max
        .max(pattern_occurrences.len());
    pattern_occurrences
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
        stats.static_walking_transfer_expansions += 1;
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
            frontier.enqueue(next, stats);
        }
    }
    false
}

fn expand_transit_rides(
    state: &StaticState,
    pattern_occurrences: &[PatternPosition],
    search: &mut PreparedSearch<'_>,
    frontier: &mut SearchFrontier,
    retention: &mut CorridorRetention,
    equivalent_legs: &mut HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    accounting: &mut SearchAccounting<'_>,
) -> bool {
    for position in pattern_occurrences {
        if deadline_exceeded(search.query) {
            accounting.stats.deadline_exceeded = true;
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
        accounting.stats.static_pattern_scans += 1;
        {
            let mode_stats = accounting.expansion_stats.modes.entry(mode).or_default();
            mode_stats.pattern_scans += 1;
            mode_stats.pattern_ids.insert(group.pattern_id.clone());
            mode_stats.stop_sequences.insert(group.stops.clone());
        }
        let profile_started = Instant::now();
        if search.patterns.ensure_ride_options(
            group_index,
            position.stop_index,
            accounting.stats,
            equivalent_legs,
            search.query.deadline,
        ) {
            accounting.stats.static_ride_profile_build_latency_us = accounting
                .stats
                .static_ride_profile_build_latency_us
                .saturating_add(elapsed_microseconds(profile_started));
        }
        if accounting.stats.deadline_exceeded {
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
                accounting.stats.deadline_exceeded = true;
                return true;
            }
            accounting.stats.static_positions_scanned += 1;
            accounting
                .expansion_stats
                .modes
                .entry(mode)
                .or_default()
                .downstream_positions_scanned += 1;
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
                accounting.stats,
                search.network,
            );
            frontier.enqueue(next, accounting.stats);
        }
    }
    false
}

fn finalize_expansion_stats(stats: &mut StaticSearchStats, expansion_stats: SearchExpansionStats) {
    stats.static_expanded_stop_occurrences_average_milli = stats
        .static_expanded_stop_occurrences_total
        .saturating_mul(1_000)
        .checked_div(stats.static_expanded_stop_occurrence_samples)
        .unwrap_or_default();
    stats.static_expanded_stop_pattern_occurrences_average_milli = stats
        .static_expanded_stop_pattern_occurrences_total
        .saturating_mul(1_000)
        .checked_div(stats.static_expanded_stop_occurrence_samples)
        .unwrap_or_default();
    stats.static_mode_stats = expansion_stats
        .modes
        .into_iter()
        .map(|(mode, accumulator)| {
            (
                mode,
                TransitModeSearchStats {
                    states_expanded: accumulator.states_expanded,
                    pattern_scans: accumulator.pattern_scans,
                    downstream_positions_scanned: accumulator.downstream_positions_scanned,
                    distinct_pattern_ids: accumulator.pattern_ids.len(),
                    distinct_stop_sequences: accumulator.stop_sequences.len(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    stats.static_expanded_states_by_leg_count = expansion_stats
        .states_by_leg_count
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect();
    stats.static_most_expanded_stops = expansion_stats.expanded_stops.into_iter().collect();
    stats
        .static_most_expanded_stops
        .sort_by(|(left_id, left_count), (right_id, right_count)| {
            right_count
                .cmp(left_count)
                .then(left_id.as_str().cmp(right_id.as_str()))
        });
    stats.static_most_expanded_stops.truncate(12);
}

fn deadline_exceeded(query: &PlanningQuery) -> bool {
    query.deadline.is_elapsed()
}

fn finish_deadline(
    started: Instant,
    mut stats: StaticSearchStats,
) -> (RetainedCorridorSet, StaticSearchStats) {
    stats.deadline_exceeded = true;
    stats.static_search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    (RetainedCorridorSet::default(), stats)
}

fn elapsed_microseconds(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
