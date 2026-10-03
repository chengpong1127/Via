use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
};

use crate::domain::ids::BoardingPointId;

use crate::routing::{
    model::PlanningQuery, topology::TransitTopologyIndex, transfers::TransferShortcutIndex,
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

pub(super) fn destination_lower_bounds(
    patterns: &StaticPhase1PatternIndex,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &PlanningQuery,
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
        if query.deadline.is_elapsed() {
            stats.deadline_exceeded = true;
            break;
        }
        if lower_bounds.get(&entry.stop_id) != Some(&entry.seconds) {
            continue;
        }

        for position in topology.occurrences_at(&entry.stop_id) {
            if query.deadline.is_elapsed() {
                stats.deadline_exceeded = true;
                break 'lower_bounds;
            }

            let Some(predecessor_index) = position.stop_index.checked_sub(1) else {
                continue;
            };
            let Some(group_index) = patterns.group_index(&position.pattern_id) else {
                continue;
            };
            let group = patterns.group(group_index);
            let predecessor = &group.stops[predecessor_index];
            let edge_seconds = group.segments[predecessor_index].seconds;
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
            if query.deadline.is_elapsed() {
                stats.deadline_exceeded = true;
                break 'lower_bounds;
            }

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
