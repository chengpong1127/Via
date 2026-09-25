//! Compact topology-only index used by both routing phases.

use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::TransitNetwork,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PatternPosition {
    pub pattern_id: TransitPatternId,
    pub stop_index: usize,
}

#[derive(Clone, Debug, Default)]
pub struct TransitTopologyIndex {
    occurrences_by_stop: HashMap<BoardingPointId, Vec<PatternPosition>>,
}

impl TransitTopologyIndex {
    pub fn new(network: &TransitNetwork) -> Self {
        let mut occurrences_by_stop = HashMap::<BoardingPointId, Vec<PatternPosition>>::new();
        for service in &network.patterns {
            for (stop_index, stop_id) in service.stops.iter().enumerate() {
                occurrences_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(PatternPosition {
                        pattern_id: service.id.clone(),
                        stop_index,
                    });
            }
        }
        for occurrences in occurrences_by_stop.values_mut() {
            occurrences.sort_by(|left, right| {
                left.pattern_id
                    .as_str()
                    .cmp(right.pattern_id.as_str())
                    .then(left.stop_index.cmp(&right.stop_index))
            });
        }
        Self {
            occurrences_by_stop,
        }
    }

    pub fn occurrences_at(&self, stop: &BoardingPointId) -> &[PatternPosition] {
        self.occurrences_by_stop
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
