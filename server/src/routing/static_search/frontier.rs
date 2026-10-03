use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
};

use crate::domain::{ids::BoardingPointId, transit::TransitMode};

use super::types::{
    ContinuationSignature, PartialSignature, StaticCorridorStep, StaticCostSource, structural_steps,
};

#[derive(Clone, Debug)]
pub(super) struct StaticState {
    pub(super) stop_id: BoardingPointId,
    pub(super) elapsed_seconds: u32,
    pub(super) lower_bound_seconds: u32,
    pub(super) priority_seconds: u32,
    pub(super) access_stop_id: BoardingPointId,
    pub(super) transit_legs_used: usize,
    pub(super) last_transit_mode: Option<TransitMode>,
    pub(super) mode_transition_sequence: Vec<TransitMode>,
    pub(super) steps: Vec<StaticCorridorStep>,
    pub(super) has_exact_cost: bool,
    pub(super) has_derived_bus_cost: bool,
    pub(super) has_estimated_cost: bool,
    pub(super) transfer_ready: bool,
}

impl StaticState {
    pub(super) fn signature(&self) -> PartialSignature {
        PartialSignature {
            access_stop_id: self.access_stop_id.clone(),
            steps: structural_steps(&self.steps),
        }
    }

    pub(super) fn continuation_signature(&self) -> ContinuationSignature {
        ContinuationSignature {
            access_stop_id: self.access_stop_id.clone(),
            mode_transition_sequence: self.mode_transition_sequence.clone(),
        }
    }

    pub(super) fn cost_source(&self) -> StaticCostSource {
        match (
            self.has_exact_cost,
            self.has_derived_bus_cost,
            self.has_estimated_cost,
        ) {
            (true, false, false) => StaticCostSource::ExactSchedule,
            (false, true, false) => StaticCostSource::DerivedBus,
            (false, false, true) | (false, false, false) => StaticCostSource::OptimisticEstimate,
            _ => StaticCostSource::Mixed,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct QueueEntry {
    pub(super) state: StaticState,
    sequence: u64,
}
impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.state.priority_seconds == other.state.priority_seconds
            && self.state.lower_bound_seconds == other.state.lower_bound_seconds
            && self.state.elapsed_seconds == other.state.elapsed_seconds
            && self.sequence == other.sequence
    }
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .state
            .priority_seconds
            .cmp(&self.state.priority_seconds)
            .then(other.state.elapsed_seconds.cmp(&self.state.elapsed_seconds))
            .then(other.sequence.cmp(&self.sequence))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DominanceKey {
    stop_id: BoardingPointId,
    transit_legs_used: usize,
    transfer_ready: bool,
}

#[derive(Clone, Debug)]
struct KnownState {
    elapsed_seconds: u32,
    signature: PartialSignature,
    continuation_signature: ContinuationSignature,
}

pub(super) struct SearchFrontier {
    queue: BinaryHeap<QueueEntry>,
    known: HashMap<DominanceKey, Vec<KnownState>>,
    sequence: u64,
    limit: usize,
}

impl SearchFrontier {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            queue: BinaryHeap::new(),
            known: HashMap::new(),
            sequence: 0,
            limit,
        }
    }

    pub(super) fn pop(&mut self) -> Option<QueueEntry> {
        self.queue.pop()
    }

    pub(super) fn enqueue(&mut self, state: StaticState) {
        let key = DominanceKey {
            stop_id: state.stop_id.clone(),
            transit_legs_used: state.transit_legs_used,
            transfer_ready: state.transfer_ready,
        };
        let signature = state.signature();
        let continuation_signature = state.continuation_signature();
        let known_states = self.known.entry(key).or_default();
        if known_states.iter().any(|known| {
            known.signature == signature && known.elapsed_seconds <= state.elapsed_seconds
        }) {
            return;
        }
        if known_states.iter().any(|known| {
            known.continuation_signature == continuation_signature
                && known.elapsed_seconds <= state.elapsed_seconds
        }) {
            return;
        }
        if known_states.len() >= self.limit
            && known_states
                .iter()
                .filter(|known| known.elapsed_seconds <= state.elapsed_seconds)
                .count()
                >= self.limit
        {
            return;
        }
        known_states.retain(|known| {
            known.signature != signature && known.continuation_signature != continuation_signature
        });
        known_states.push(KnownState {
            elapsed_seconds: state.elapsed_seconds,
            signature,
            continuation_signature,
        });
        known_states.sort_by_key(|known| known.elapsed_seconds);
        known_states.truncate(self.limit);
        self.queue.push(QueueEntry {
            state,
            sequence: self.sequence,
        });
        self.sequence = self.sequence.saturating_add(1);
    }

    pub(super) fn is_current(&self, state: &StaticState) -> bool {
        let key = DominanceKey {
            stop_id: state.stop_id.clone(),
            transit_legs_used: state.transit_legs_used,
            transfer_ready: state.transfer_ready,
        };
        let signature = state.signature();
        self.known.get(&key).is_some_and(|known_states| {
            known_states.iter().any(|known| {
                known.signature == signature && known.elapsed_seconds == state.elapsed_seconds
            })
        })
    }
}
