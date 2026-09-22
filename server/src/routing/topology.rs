//! Compact topology-only index used by both routing phases.

use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitServiceId},
    transit::TransitNetwork,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServicePosition {
    pub service_id: TransitServiceId,
    pub stop_index: usize,
}

#[derive(Clone, Debug, Default)]
pub struct TransitTopologyIndex {
    occurrences_by_stop: HashMap<BoardingPointId, Vec<ServicePosition>>,
}

impl TransitTopologyIndex {
    pub fn new(network: &TransitNetwork) -> Self {
        let mut occurrences_by_stop = HashMap::<BoardingPointId, Vec<ServicePosition>>::new();
        for service in &network.services {
            for (stop_index, stop_id) in service.pattern.iter().enumerate() {
                occurrences_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(ServicePosition {
                        service_id: service.id.clone(),
                        stop_index,
                    });
            }
        }
        for occurrences in occurrences_by_stop.values_mut() {
            occurrences.sort_by(|left, right| {
                left.service_id
                    .as_str()
                    .cmp(right.service_id.as_str())
                    .then(left.stop_index.cmp(&right.stop_index))
            });
        }
        Self {
            occurrences_by_stop,
        }
    }

    pub fn occurrences_at(&self, stop: &BoardingPointId) -> &[ServicePosition] {
        self.occurrences_by_stop
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
