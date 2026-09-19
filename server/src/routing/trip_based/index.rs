use crate::domain::{
    ids::{BoardingPointId, TransitServiceId},
    transit::TransitNetwork,
};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    pattern: Vec<BoardingPointId>,
    service_ids: Vec<TransitServiceId>,
}

/// Static Trip-Based index. It contains only service patterns and stable
/// schedule ordering; runtime trips are supplied by `DepartureResolver`.
#[derive(Clone, Debug, Default)]
pub(super) struct TripBasedIndex {
    lines: Vec<Line>,
    service_to_line: HashMap<TransitServiceId, usize>,
    services_by_stop: HashMap<BoardingPointId, Vec<TransitServiceId>>,
    stop_indexes: HashMap<TransitServiceId, HashMap<BoardingPointId, usize>>,
}

impl TripBasedIndex {
    pub(super) fn new(network: &TransitNetwork) -> Self {
        let mut lines = Vec::<Line>::new();
        let mut service_to_line = HashMap::new();
        let mut services_by_stop = HashMap::<BoardingPointId, Vec<TransitServiceId>>::new();
        let mut stop_indexes = HashMap::<TransitServiceId, HashMap<BoardingPointId, usize>>::new();
        for service in &network.services {
            let line_index = lines
                .iter()
                .position(|line| line.pattern == service.pattern)
                .unwrap_or_else(|| {
                    lines.push(Line {
                        pattern: service.pattern.clone(),
                        service_ids: Vec::new(),
                    });
                    lines.len() - 1
                });
            lines[line_index].service_ids.push(service.id.clone());
            service_to_line.insert(service.id.clone(), line_index);
            for (stop_index, stop_id) in service.pattern.iter().enumerate() {
                services_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(service.id.clone());
                stop_indexes
                    .entry(service.id.clone())
                    .or_default()
                    .entry(stop_id.clone())
                    .or_insert(stop_index);
            }
        }
        for line in &mut lines {
            line.service_ids
                .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        for service_ids in services_by_stop.values_mut() {
            service_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            service_ids.dedup();
        }
        Self {
            lines,
            service_to_line,
            services_by_stop,
            stop_indexes,
        }
    }

    fn line_for_service(&self, service_id: &TransitServiceId) -> Option<&Line> {
        self.service_to_line
            .get(service_id)
            .and_then(|index| self.lines.get(*index))
    }

    pub(super) fn services_at(&self, stop_id: &BoardingPointId) -> &[TransitServiceId] {
        self.services_by_stop
            .get(stop_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(super) fn stop_index(
        &self,
        service_id: &TransitServiceId,
        stop_id: &BoardingPointId,
    ) -> Option<usize> {
        self.stop_indexes
            .get(service_id)
            .and_then(|stop_indexes| stop_indexes.get(stop_id))
            .copied()
    }

    pub(super) fn pattern(&self, service_id: &TransitServiceId) -> Option<&[BoardingPointId]> {
        self.line_for_service(service_id)
            .map(|line| line.pattern.as_slice())
    }
}
