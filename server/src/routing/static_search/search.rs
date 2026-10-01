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
    model::TransitQuery, topology::TransitTopologyIndex, transfers::TransferShortcutIndex,
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

pub fn generate(
    network: &TransitRuntime,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &TransitQuery,
) -> (RetainedCorridorSet, StaticSearchStats) {
    let started = Instant::now();
    let limit = query.limits.max_static_corridors.max(1);
    let max_legs = query.limits.max_transfers.saturating_add(1);
    let mut stats = StaticSearchStats::default();
    if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
        stats.deadline_exceeded = true;
        stats.static_search_latency_ms =
            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        return (RetainedCorridorSet::default(), stats);
    }
    let pattern_index_started = Instant::now();
    let mut patterns = StaticPhase1PatternIndex::new(
        network,
        &query.allowed_transit_modes,
        &mut stats,
        query.deadline,
    );
    stats.static_pattern_index_build_latency_us = elapsed_microseconds(pattern_index_started);
    if stats.deadline_exceeded || query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
        stats.deadline_exceeded = true;
        stats.static_search_latency_ms =
            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        return (RetainedCorridorSet::default(), stats);
    }
    let egress_by_stop = query.destination_egress.iter().fold(
        HashMap::<BoardingPointId, Vec<u32>>::new(),
        |mut by_stop, egress| {
            by_stop
                .entry(egress.stop_id.clone())
                .or_default()
                .push(egress.walking_seconds);
            by_stop
        },
    );
    let terminal_transfer_source_stops = egress_by_stop
        .keys()
        .flat_map(|egress_stop| transfers.transfers_to(egress_stop))
        .map(|shortcut| shortcut.from.clone())
        .collect::<HashSet<_>>();
    let lower_bound_started = Instant::now();
    let destination_lower_bounds =
        destination_lower_bounds(&patterns, topology, transfers, query, &mut stats);
    stats.static_lower_bound_latency_us = elapsed_microseconds(lower_bound_started);
    if stats.deadline_exceeded || query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
        stats.deadline_exceeded = true;
        stats.static_search_latency_ms =
            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        return (RetainedCorridorSet::default(), stats);
    }
    let Some(required_mode_pairs) = reachable_endpoint_mode_pairs(
        network,
        topology,
        transfers,
        query,
        &destination_lower_bounds,
    ) else {
        stats.deadline_exceeded = true;
        stats.static_search_latency_ms =
            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        return (RetainedCorridorSet::default(), stats);
    };
    if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
        stats.deadline_exceeded = true;
        stats.static_search_latency_ms =
            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        return (RetainedCorridorSet::default(), stats);
    }
    stats.static_required_endpoint_mode_pairs = sorted_mode_pairs(&required_mode_pairs);
    let mut frontier = SearchFrontier::new(limit);
    for access in &query.origin_access {
        let Some(remaining_seconds) = destination_lower_bounds.get(&access.stop_id).copied() else {
            continue;
        };
        let elapsed_seconds = access.walking_seconds;
        let state = StaticState {
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
        };
        frontier.enqueue(state, &mut stats);
    }

    let forward_search_started = Instant::now();
    let mut retention = CorridorRetention::new(limit);
    let mut equivalent_legs = HashMap::<EquivalentLegKey, Vec<StaticTransitLeg>>::new();
    let mut expanded_states_by_leg_count = HashMap::<usize, usize>::new();
    let mut expanded_stops = HashMap::<BoardingPointId, usize>::new();
    let mut mode_accumulators = HashMap::<TransitMode, ModeSearchAccumulator>::new();
    'search: while let Some(entry) = frontier.pop() {
        if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
            stats.deadline_exceeded = true;
            break;
        }
        if retention
            .kth_duration()
            .is_some_and(|kth| entry.state.priority_seconds > kth)
        {
            stats.static_top_k_early_terminated = true;
            break;
        }
        if !frontier.is_current(&entry.state) {
            continue;
        }
        if stats.static_search_states_expanded >= query.limits.max_phase1_expansions {
            stats.static_search_limit_reached = true;
            break;
        }
        stats.static_search_states_expanded += 1;
        let state = entry.state;
        *expanded_states_by_leg_count
            .entry(state.transit_legs_used)
            .or_default() += 1;
        *expanded_stops.entry(state.stop_id.clone()).or_default() += 1;
        if egress_by_stop.contains_key(&state.stop_id) {
            stats.static_expanded_egress_states += 1;
        }
        if terminal_transfer_source_stops.contains(&state.stop_id) {
            stats.static_expanded_terminal_transfer_source_states += 1;
        }
        if let Some(mode) = state.last_transit_mode {
            mode_accumulators.entry(mode).or_default().states_expanded += 1;
        } else {
            stats.static_unattributed_states_expanded += 1;
        }
        let occurrences = topology.occurrences_at(&state.stop_id);
        let pattern_occurrences = occurrences
            .iter()
            .filter(|position| patterns.group_index(&position.pattern_id).is_some())
            .cloned()
            .collect::<Vec<_>>();
        stats.static_expanded_stop_occurrence_samples += 1;
        stats.static_expanded_stop_occurrences_total += occurrences.len();
        stats.static_expanded_stop_pattern_occurrences_total += pattern_occurrences.len();
        stats.static_expanded_stop_occurrences_max = stats
            .static_expanded_stop_occurrences_max
            .max(occurrences.len());
        stats.static_expanded_stop_pattern_occurrences_max = stats
            .static_expanded_stop_pattern_occurrences_max
            .max(pattern_occurrences.len());

        if !state.transfer_ready {
            for shortcut in transfers.transfers_from(&state.stop_id) {
                if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                    stats.deadline_exceeded = true;
                    break 'search;
                }
                stats.static_walking_transfer_expansions += 1;
                let elapsed_seconds = state
                    .elapsed_seconds
                    .saturating_add(shortcut.duration_seconds);
                if elapsed_seconds > query.limits.max_journey_duration_seconds {
                    continue;
                }
                let mut next = state.clone();
                next.stop_id = shortcut.to.clone();
                next.elapsed_seconds = elapsed_seconds;
                let Some(remaining_seconds) = destination_lower_bounds.get(&shortcut.to).copied()
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
                        &egress_by_stop,
                        query.limits.max_journey_duration_seconds,
                        &mut stats,
                        network,
                    );
                }
                if next.transit_legs_used < max_legs {
                    frontier.enqueue(next, &mut stats);
                }
            }
        }

        if state.transit_legs_used >= max_legs {
            continue;
        }

        for position in pattern_occurrences {
            if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                stats.deadline_exceeded = true;
                break 'search;
            }
            let group_index = patterns
                .group_index(&position.pattern_id)
                .expect("indexed pattern");
            let group = patterns.group(group_index);
            let mode = group.mode;
            if !query.allowed_transit_modes.contains(&mode) {
                continue;
            }
            stats.static_pattern_scans += 1;
            {
                let mode_stats = mode_accumulators.entry(mode).or_default();
                mode_stats.pattern_scans += 1;
                mode_stats.pattern_ids.insert(group.pattern_id.clone());
                mode_stats.stop_sequences.insert(group.stops.clone());
            }
            let profile_started = Instant::now();
            if patterns.ensure_ride_options(
                group_index,
                position.stop_index,
                &mut stats,
                &mut equivalent_legs,
                query.deadline,
            ) {
                stats.static_ride_profile_build_latency_us = stats
                    .static_ride_profile_build_latency_us
                    .saturating_add(elapsed_microseconds(profile_started));
            }
            if stats.deadline_exceeded {
                break 'search;
            }
            let transfer_seconds = if state.transit_legs_used > 0 && !state.transfer_ready {
                network.same_stop_transfer_seconds()
            } else {
                0
            };
            for ride_option in patterns.ride_options(group_index, position.stop_index) {
                if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                    stats.deadline_exceeded = true;
                    break 'search;
                }
                stats.static_positions_scanned += 1;
                mode_accumulators
                    .entry(mode)
                    .or_default()
                    .downstream_positions_scanned += 1;
                let Some(alternative) = ride_option.alternatives.first() else {
                    continue;
                };
                stats.static_structurally_equivalent_states_rejected +=
                    ride_option.alternatives.len().saturating_sub(1);
                let ride_seconds = alternative.duration_seconds;
                let source = alternative.leg.cost_source;
                let elapsed_seconds = state
                    .elapsed_seconds
                    .saturating_add(transfer_seconds)
                    .saturating_add(ride_seconds);
                if elapsed_seconds > query.limits.max_journey_duration_seconds {
                    continue;
                }
                let alight_stop_id = ride_option.alight_stop_id.clone();
                let mut next = state.clone();
                next.stop_id = alight_stop_id.clone();
                next.elapsed_seconds = elapsed_seconds;
                let Some(remaining_seconds) =
                    destination_lower_bounds.get(&alight_stop_id).copied()
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
                    .push(StaticCorridorStep::Transit(alternative.leg.clone()));
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
                    &egress_by_stop,
                    query.limits.max_journey_duration_seconds,
                    &mut stats,
                    network,
                );
                frontier.enqueue(next, &mut stats);
            }
        }
    }

    if stats.deadline_exceeded || query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
        stats.deadline_exceeded = true;
        stats.static_forward_search_latency_us = elapsed_microseconds(forward_search_started);
        stats.static_search_latency_ms =
            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        return (RetainedCorridorSet::default(), stats);
    }

    stats.static_forward_search_latency_us = elapsed_microseconds(forward_search_started);
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
    stats.static_mode_stats = mode_accumulators
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
    stats.static_expanded_states_by_leg_count = expanded_states_by_leg_count
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect();
    stats.static_most_expanded_stops = expanded_stops.into_iter().collect();
    stats
        .static_most_expanded_stops
        .sort_by(|(left_id, left_count), (right_id, right_count)| {
            right_count
                .cmp(left_count)
                .then(left_id.as_str().cmp(right_id.as_str()))
        });
    stats.static_most_expanded_stops.truncate(12);

    let retention_started = Instant::now();
    let surface = retention.finish(network, &equivalent_legs, &mut stats);
    stats.static_retention_latency_us = elapsed_microseconds(retention_started);
    stats.static_search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    (surface, stats)
}

fn elapsed_microseconds(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
