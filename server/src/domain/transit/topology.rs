use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::network::{SegmentTravelTime, TransitMode, TransitNetwork},
};

pub type StopIndex = u32;
pub type PatternIndex = u32;
pub type PlaceIndex = u32;

const PLACE_RADIUS_METRES: f64 = 100.0;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DensePattern {
    pub mode: TransitMode,
    pub line: u64,
    pub stops: Vec<StopIndex>,
    pub segments: Vec<SegmentTravelTime>,
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
    place_by_stop: Vec<PlaceIndex>,
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
                mode,
                line,
                stops: pattern.stops.iter().map(|id| stop_by_id[id]).collect(),
                segments: pattern.static_profile.segments.clone(),
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
        let mut edge_order = (0..network.transfer_edges.len()).collect::<Vec<_>>();
        edge_order.sort_by_key(|index| {
            let edge = &network.transfer_edges[*index];
            (
                stop_by_id[&edge.from],
                stop_by_id[&edge.to],
                edge.duration_seconds,
                edge.distance_metres,
                *index,
            )
        });
        edge_order.dedup_by(|later, earlier| {
            network.transfer_edges[*later] == network.transfer_edges[*earlier]
        });
        edge_order.sort_by_key(|index| {
            let edge = &network.transfer_edges[*index];
            (
                stop_by_id[&edge.from],
                stop_by_id[&edge.to],
                edge.duration_seconds,
                *index,
            )
        });
        for edge_index in edge_order {
            let edge = &network.transfer_edges[edge_index];
            transfers_by_stop[stop_by_id[&edge.from] as usize].push(DenseTransfer {
                to: stop_by_id[&edge.to],
                duration_seconds: edge.duration_seconds,
                edge: edge_index as u32,
            });
        }

        let place_by_stop = places(network, &stop_ids);

        Self {
            stop_ids,
            stop_by_id,
            pattern_ids,
            pattern_by_id,
            patterns,
            positions_by_stop,
            transfers_by_stop,
            place_by_stop,
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

    pub fn place(&self, stop: StopIndex) -> PlaceIndex {
        self.place_by_stop[stop as usize]
    }
}

fn places(network: &TransitNetwork, stop_ids: &[BoardingPointId]) -> Vec<PlaceIndex> {
    let stop_by_id = network
        .stops
        .iter()
        .map(|stop| (&stop.id, stop))
        .collect::<HashMap<_, _>>();
    let mut stops_by_name = HashMap::<&str, Vec<usize>>::new();
    for (index, id) in stop_ids.iter().enumerate() {
        if let Some(stop) = stop_by_id.get(id) {
            stops_by_name
                .entry(stop.name.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut root = (0..stop_ids.len()).collect::<Vec<_>>();
    fn find(root: &mut [usize], mut index: usize) -> usize {
        while root[index] != index {
            root[index] = root[root[index]];
            index = root[index];
        }
        index
    }
    for members in stops_by_name.values() {
        for (position, &left) in members.iter().enumerate() {
            for &right in &members[position + 1..] {
                let distance = stop_by_id[&stop_ids[left]]
                    .coordinate
                    .distance_metres(stop_by_id[&stop_ids[right]].coordinate);
                if distance <= PLACE_RADIUS_METRES {
                    let (left_root, right_root) = (find(&mut root, left), find(&mut root, right));
                    root[left_root.max(right_root)] = left_root.min(right_root);
                }
            }
        }
    }
    (0..stop_ids.len())
        .map(|index| find(&mut root, index) as PlaceIndex)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::domain::transit::{
        network::{TransferEdge, TransitNetwork},
        runtime::TransitRuntime,
    };

    fn runtime() -> TransitRuntime {
        let mut network = TransitNetwork::from_artifact_json(include_str!(
            "../../../fixtures/routing/network.json"
        ))
        .unwrap();
        let first = network.transfer_edges[0].clone();
        let mut longer = first.clone();
        longer.distance_metres += 40;
        let mut reversed = first.clone();
        std::mem::swap(&mut reversed.from, &mut reversed.to);
        network
            .transfer_edges
            .extend([longer.clone(), first.clone(), reversed, longer]);
        network.transfer_edges.push(TransferEdge {
            duration_seconds: first.duration_seconds + 30,
            ..first
        });
        TransitRuntime::new(Arc::new(network))
    }

    #[test]
    fn dense_indices_follow_sorted_ids() {
        let transit = runtime();
        let dense = transit.dense();
        let stops = (0..dense.stop_count() as u32)
            .map(|index| dense.stop_id(index).as_str())
            .collect::<Vec<_>>();
        assert!(stops.windows(2).all(|pair| pair[0] < pair[1]));
        let patterns = (0..transit.network().patterns.len() as u32)
            .map(|index| dense.pattern_id(index).as_str())
            .collect::<Vec<_>>();
        assert!(patterns.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn dense_topology_matches_string_topology() {
        let transit = runtime();
        let dense = transit.dense();
        for index in 0..dense.stop_count() as u32 {
            let stop_id = dense.stop_id(index);
            let positions = dense
                .positions_at(index)
                .iter()
                .map(|position| {
                    (
                        dense.pattern_id(position.pattern).clone(),
                        position.stop_index as usize,
                    )
                })
                .collect::<Vec<_>>();
            let expected_positions = transit
                .pattern_positions_at(stop_id)
                .iter()
                .map(|position| (position.pattern_id.clone(), position.stop_index))
                .collect::<Vec<_>>();
            assert_eq!(positions, expected_positions);
            let transfers = dense
                .transfers_from(index)
                .iter()
                .map(|transfer| {
                    let edge = &transit.network().transfer_edges[transfer.edge as usize];
                    assert_eq!(dense.stop_id(transfer.to), &edge.to);
                    assert_eq!(transfer.duration_seconds, edge.duration_seconds);
                    edge.clone()
                })
                .collect::<Vec<_>>();
            assert_eq!(transfers, transit.transfers_from(stop_id));
        }
    }
}
