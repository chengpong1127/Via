use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap, HashSet},
};

use crate::domain::{ids::BoardingPointId, transit::TransitMode, transit_runtime::TransitRuntime};

use crate::routing::{
    model::TransitQuery, topology::TransitTopologyIndex, transfers::TransferShortcutIndex,
};

use super::{pattern_index::StaticPhase1PatternIndex, types::StaticSearchStats};

#[derive(Clone, Debug, PartialEq, Eq)]
struct LowerBoundEntry {
    stop_id: BoardingPointId,
    seconds: u32,
    sequence: u64,
}

impl PartialOrd for LowerBoundEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LowerBoundEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .seconds
            .cmp(&self.seconds)
            .then(other.sequence.cmp(&self.sequence))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct EndpointModePair {
    pub(super) first: TransitMode,
    pub(super) last: TransitMode,
}

pub(super) fn destination_lower_bounds(
    patterns: &StaticPhase1PatternIndex,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &TransitQuery,
    stats: &mut StaticSearchStats,
) -> HashMap<BoardingPointId, u32> {
    let mut lower_bounds = HashMap::<BoardingPointId, u32>::new();
    let mut frontier = BinaryHeap::new();
    let mut sequence = 0_u64;
    for egress in &query.destination_egress {
        if egress.walking_seconds > query.limits.max_journey_duration_seconds {
            continue;
        }
        if lower_bounds
            .get(&egress.stop_id)
            .is_some_and(|known| *known <= egress.walking_seconds)
        {
            continue;
        }
        lower_bounds.insert(egress.stop_id.clone(), egress.walking_seconds);
        frontier.push(LowerBoundEntry {
            stop_id: egress.stop_id.clone(),
            seconds: egress.walking_seconds,
            sequence,
        });
        sequence = sequence.saturating_add(1);
    }

    'lower_bounds: while let Some(entry) = frontier.pop() {
        if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
            stats.deadline_exceeded = true;
            break;
        }
        if lower_bounds.get(&entry.stop_id) != Some(&entry.seconds) {
            continue;
        }
        stats.static_lower_bound_nodes_settled += 1;
        for position in topology.occurrences_at(&entry.stop_id) {
            if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                stats.deadline_exceeded = true;
                break 'lower_bounds;
            }
            stats.static_lower_bound_pattern_edges_scanned += 1;
            let Some(predecessor_index) = position.stop_index.checked_sub(1) else {
                continue;
            };
            let Some(group_index) = patterns.group_index(&position.pattern_id) else {
                continue;
            };
            let group = patterns.group(group_index);
            let Some(predecessor) = group.stops.get(predecessor_index) else {
                continue;
            };
            let Some(edge_seconds) = group
                .segments
                .get(predecessor_index)
                .map(|segment| segment.seconds)
            else {
                continue;
            };
            relax_lower_bound(
                predecessor,
                entry.seconds.saturating_add(edge_seconds),
                &mut lower_bounds,
                &mut frontier,
                &mut sequence,
                query.limits.max_journey_duration_seconds,
            );
        }
        for shortcut in transfers.transfers_to(&entry.stop_id) {
            if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                stats.deadline_exceeded = true;
                break 'lower_bounds;
            }
            stats.static_lower_bound_transfer_edges_scanned += 1;
            relax_lower_bound(
                &shortcut.from,
                entry.seconds.saturating_add(shortcut.duration_seconds),
                &mut lower_bounds,
                &mut frontier,
                &mut sequence,
                query.limits.max_journey_duration_seconds,
            );
        }
    }
    lower_bounds
}

fn relax_lower_bound(
    stop_id: &BoardingPointId,
    seconds: u32,
    lower_bounds: &mut HashMap<BoardingPointId, u32>,
    frontier: &mut BinaryHeap<LowerBoundEntry>,
    sequence: &mut u64,
    maximum_seconds: u32,
) {
    if seconds > maximum_seconds {
        return;
    }
    if lower_bounds
        .get(stop_id)
        .is_some_and(|known| *known <= seconds)
    {
        return;
    }
    lower_bounds.insert(stop_id.clone(), seconds);
    frontier.push(LowerBoundEntry {
        stop_id: stop_id.clone(),
        seconds,
        sequence: *sequence,
    });
    *sequence = sequence.saturating_add(1);
}

pub(super) fn reachable_endpoint_mode_pairs(
    network: &TransitRuntime,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &TransitQuery,
    destination_lower_bounds: &HashMap<BoardingPointId, u32>,
) -> Option<HashSet<EndpointModePair>> {
    if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
        return None;
    }
    let mut origin_modes = HashSet::new();
    for access in &query.origin_access {
        if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
            return None;
        }
        let mut boarding_stops = vec![access.stop_id.clone()];
        boarding_stops.extend(
            transfers
                .transfers_from(&access.stop_id)
                .iter()
                .map(|shortcut| shortcut.to.clone()),
        );
        for stop_id in boarding_stops {
            if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                return None;
            }
            if !destination_lower_bounds.contains_key(&stop_id) {
                continue;
            }
            for (index, position) in topology.occurrences_at(&stop_id).iter().enumerate() {
                if index % 64 == 0 && query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                    return None;
                }
                let Some(pattern) = network.pattern(&position.pattern_id) else {
                    continue;
                };
                if query
                    .allowed_transit_modes
                    .contains(&network.route_of(pattern).mode)
                {
                    origin_modes.insert(network.route_of(pattern).mode);
                }
            }
        }
    }

    let mut destination_modes = HashSet::new();
    for egress in &query.destination_egress {
        if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
            return None;
        }
        let mut alighting_stops = vec![egress.stop_id.clone()];
        alighting_stops.extend(
            transfers
                .transfers_to(&egress.stop_id)
                .iter()
                .map(|shortcut| shortcut.from.clone()),
        );
        for stop_id in alighting_stops {
            if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                return None;
            }
            for (index, position) in topology.occurrences_at(&stop_id).iter().enumerate() {
                if index % 64 == 0 && query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
                    return None;
                }
                let Some(pattern) = network.pattern(&position.pattern_id) else {
                    continue;
                };
                if query
                    .allowed_transit_modes
                    .contains(&network.route_of(pattern).mode)
                {
                    destination_modes.insert(network.route_of(pattern).mode);
                }
            }
        }
    }

    Some(
        origin_modes
            .into_iter()
            .flat_map(|first| {
                destination_modes
                    .iter()
                    .copied()
                    .map(move |last| EndpointModePair { first, last })
            })
            .collect(),
    )
}

pub(super) fn sorted_mode_pairs(
    pairs: &HashSet<EndpointModePair>,
) -> Vec<(TransitMode, TransitMode)> {
    let mut pairs = pairs
        .iter()
        .map(|pair| (pair.first, pair.last))
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
}
