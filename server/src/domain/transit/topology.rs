use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::network::{SegmentTimeSource, TransitMode, TransitNetwork},
};

pub type StopIndex = u32;
pub type PatternIndex = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DensePosition {
    pub pattern: PatternIndex,
    pub stop_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DenseTransfer {
    pub to: StopIndex,
    pub duration_seconds: u32,
    pub edge: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DenseSegment {
    pub seconds: u32,
    pub source: SegmentTimeSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DensePattern {
    pub network_position: usize,
    pub mode: TransitMode,
    pub line: u64,
    pub stops: Vec<StopIndex>,
    pub segments: Vec<DenseSegment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DenseTopology {
    stop_ids: Vec<BoardingPointId>,
    stop_by_id: HashMap<BoardingPointId, StopIndex>,
    pattern_ids: Vec<TransitPatternId>,
    pattern_by_id: HashMap<TransitPatternId, PatternIndex>,
    patterns: Vec<DensePattern>,
    positions_by_stop: Vec<Vec<DensePosition>>,
    transfers_by_stop: Vec<Vec<DenseTransfer>>,
}

impl DenseTopology {
    pub(super) fn build(network: &TransitNetwork) -> Self {
        let mut stop_ids = network
            .stops
            .iter()
            .map(|stop| stop.id.clone())
            .chain(
                network
                    .patterns
                    .iter()
                    .flat_map(|pattern| pattern.stops.iter().cloned()),
            )
            .chain(
                network
                    .transfer_edges
                    .iter()
                    .flat_map(|edge| [edge.from.clone(), edge.to.clone()]),
            )
            .collect::<Vec<_>>();
        stop_ids.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
        stop_ids.dedup();
        let stop_by_id = stop_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.clone(), index as StopIndex))
            .collect::<HashMap<_, _>>();

        let route_position_by_id = network
            .routes
            .iter()
            .enumerate()
            .map(|(index, route)| (&route.id, index))
            .collect::<HashMap<_, _>>();
        let mut pattern_order = (0..network.patterns.len()).collect::<Vec<_>>();
        pattern_order.sort_by(|left, right| {
            network.patterns[*left]
                .id
                .as_str()
                .cmp(network.patterns[*right].id.as_str())
        });
        let mut pattern_ids = Vec::with_capacity(pattern_order.len());
        let mut patterns = Vec::with_capacity(pattern_order.len());
        for network_position in pattern_order {
            let pattern = &network.patterns[network_position];
            let Some(route_position) = route_position_by_id.get(&pattern.route_id) else {
                continue;
            };
            let mode = network.routes[*route_position].mode;
            let pattern_index = patterns.len() as u64;
            let line = match mode {
                TransitMode::Rail | TransitMode::HighSpeedRail => (1 << 32) | pattern_index,
                TransitMode::Bus | TransitMode::Metro => *route_position as u64,
            };
            pattern_ids.push(pattern.id.clone());
            patterns.push(DensePattern {
                network_position,
                mode,
                line,
                stops: pattern.stops.iter().map(|id| stop_by_id[id]).collect(),
                segments: pattern
                    .static_profile
                    .segments
                    .iter()
                    .map(|segment| DenseSegment {
                        seconds: segment.seconds,
                        source: segment.source,
                    })
                    .collect(),
            });
        }
        let pattern_by_id = pattern_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.clone(), index as PatternIndex))
            .collect::<HashMap<_, _>>();

        let mut positions_by_stop = vec![Vec::new(); stop_ids.len()];
        for (pattern_index, pattern) in patterns.iter().enumerate() {
            for (stop_index, stop) in pattern.stops.iter().enumerate() {
                positions_by_stop[*stop as usize].push(DensePosition {
                    pattern: pattern_index as PatternIndex,
                    stop_index: stop_index as u32,
                });
            }
        }

        let mut transfers_by_stop = vec![Vec::<DenseTransfer>::new(); stop_ids.len()];
        let mut seen = vec![Vec::<usize>::new(); stop_ids.len()];
        for (edge_index, edge) in network.transfer_edges.iter().enumerate() {
            let from = stop_by_id[&edge.from] as usize;
            if seen[from]
                .iter()
                .any(|known| network.transfer_edges[*known] == *edge)
            {
                continue;
            }
            seen[from].push(edge_index);
            transfers_by_stop[from].push(DenseTransfer {
                to: stop_by_id[&edge.to],
                duration_seconds: edge.duration_seconds,
                edge: edge_index as u32,
            });
        }
        for outgoing in &mut transfers_by_stop {
            outgoing.sort_by(|left, right| {
                left.to
                    .cmp(&right.to)
                    .then(left.duration_seconds.cmp(&right.duration_seconds))
            });
        }

        Self {
            stop_ids,
            stop_by_id,
            pattern_ids,
            pattern_by_id,
            patterns,
            positions_by_stop,
            transfers_by_stop,
        }
    }

    pub fn stop_index(&self, id: &BoardingPointId) -> Option<StopIndex> {
        self.stop_by_id.get(id).copied()
    }

    pub fn stop_id(&self, index: StopIndex) -> &BoardingPointId {
        &self.stop_ids[index as usize]
    }

    pub fn stop_count(&self) -> usize {
        self.stop_ids.len()
    }

    pub fn pattern_index(&self, id: &TransitPatternId) -> Option<PatternIndex> {
        self.pattern_by_id.get(id).copied()
    }

    pub fn pattern_id(&self, index: PatternIndex) -> &TransitPatternId {
        &self.pattern_ids[index as usize]
    }

    pub fn pattern(&self, index: PatternIndex) -> &DensePattern {
        &self.patterns[index as usize]
    }

    pub fn positions_at(&self, stop: StopIndex) -> &[DensePosition] {
        &self.positions_by_stop[stop as usize]
    }

    pub fn transfers_from(&self, stop: StopIndex) -> &[DenseTransfer] {
        &self.transfers_by_stop[stop as usize]
    }
}
