use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::network::{SegmentTimeSource, TransitMode, TransitPattern},
    transit::runtime::TransitRuntime,
    transit::topology::{DenseTopology, PatternIndex, StopIndex},
};

use crate::domain::ranking::TransferKind;
use crate::routing::model::{EndpointStop, PlannerLimits, PlanningQuery, RoutingError};

use super::{
    corridor::Corridor,
    types::{
        CorridorSearchStats, EquivalentCorridorSignature, EquivalentLegKey, StaticCorridorStep,
        StaticCostSource, StaticTransitLeg, equivalent_leg_key, equivalent_steps,
    },
};

const LABELS_PER_STOP: usize = 4;
const ROUTE_VARIANTS_PER_STRUCTURE: usize = 2;
const TARGET_SLACK_PERCENT: u32 = 50;
const TARGET_SLACK_SECONDS: u32 = 600;
const ORIGIN_TAG: u64 = 1 << 62;
const TRANSFER_TAG: u64 = 1 << 61;

#[derive(Clone, Copy)]
struct Label {
    cost_seconds: u32,
    penalty_seconds: u32,
    last_mode: Option<TransitMode>,
    node: u32,
    structure: u64,
    last_line: Option<u64>,
    after_ride: bool,
}

#[derive(Clone, Copy)]
enum NodeStep {
    Transit {
        pattern: PatternIndex,
        board_index: u32,
        alight_index: u32,
        cost_source: StaticCostSource,
    },
    Transfer {
        edge: u32,
    },
}

struct Node {
    parent: Option<u32>,
    access_stop: StopIndex,
    step: Option<NodeStep>,
}

struct Boarded {
    label: Label,
    board_index: u32,
    board_cost_seconds: u32,
    penalty_seconds: u32,
    ride_seconds: u32,
    cost_source: CostSourceTracker,
}

#[derive(Clone, Copy, Default)]
struct CostSourceTracker(Option<StaticCostSource>);

impl CostSourceTracker {
    fn push(&mut self, source: StaticCostSource) {
        self.0 = Some(match self.0 {
            Some(known) if known != source => StaticCostSource::Mixed,
            _ => source,
        });
    }

    fn push_segment(&mut self, source: SegmentTimeSource) {
        self.push(match source {
            SegmentTimeSource::Schedule => StaticCostSource::ExactSchedule,
            SegmentTimeSource::Derived => StaticCostSource::DerivedBus,
            SegmentTimeSource::Estimated => StaticCostSource::OptimisticEstimate,
        });
    }

    fn finish(self) -> StaticCostSource {
        self.0.unwrap_or(StaticCostSource::OptimisticEstimate)
    }
}

struct Candidate {
    rides: usize,
    search_cost_seconds: u32,
    theoretical_duration_seconds: u32,
    access_stop_id: BoardingPointId,
    egress_stop_id: BoardingPointId,
    steps: Vec<StaticCorridorStep>,
}

impl Candidate {
    fn transit_legs(&self) -> impl Iterator<Item = &StaticTransitLeg> {
        self.steps.iter().filter_map(|step| match step {
            StaticCorridorStep::Transit(leg) => Some(leg),
            StaticCorridorStep::SurfaceTransfer { .. } => None,
        })
    }

    fn pattern_sequence(&self) -> Vec<TransitPatternId> {
        self.transit_legs()
            .map(|leg| leg.pattern_id.clone())
            .collect()
    }
}

struct RoundLabels {
    labels_by_stop: Vec<Vec<Label>>,
    touched: Vec<StopIndex>,
}

impl RoundLabels {
    fn new(stop_count: usize) -> Self {
        Self {
            labels_by_stop: vec![Vec::new(); stop_count],
            touched: Vec::new(),
        }
    }

    fn push(&mut self, stop: StopIndex, label: Label) {
        let labels = &mut self.labels_by_stop[stop as usize];
        if labels.is_empty() {
            self.touched.push(stop);
        }
        labels.push(label);
    }

    fn get(&self, stop: StopIndex) -> &[Label] {
        &self.labels_by_stop[stop as usize]
    }

    fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }

    fn sort_stops(&mut self) {
        self.touched.sort_unstable();
    }

    fn clear(&mut self) {
        for stop in self.touched.drain(..) {
            self.labels_by_stop[stop as usize].clear();
        }
    }
}

struct Search<'a> {
    transit: &'a TransitRuntime,
    dense: &'a DenseTopology,
    limits: PlannerLimits,
    nodes: Vec<Node>,
    entries_by_stop: Vec<Vec<BagEntry>>,
    target_seconds: u32,
    stats: CorridorSearchStats,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BagEntry {
    cost_seconds: u32,
    structure: u64,
    last_line: Option<u64>,
}

pub fn generate(
    transit: &TransitRuntime,
    query: &PlanningQuery,
) -> Result<(Vec<Corridor>, CorridorSearchStats), RoutingError> {
    if query.allowed_transit_modes.is_empty()
        || query.origin_access.is_empty()
        || query.destination_egress.is_empty()
    {
        return Ok((Vec::new(), CorridorSearchStats::default()));
    }
    let started = Instant::now();
    let dense = transit.dense();
    let egress_seconds_by_stop = egress_seconds_by_stop(dense, &query.destination_egress);
    let mut search = Search::new(transit, query.limits);
    let mut current = search.seed_access(&query.origin_access);
    let mut next = RoundLabels::new(dense.stop_count());
    search.relax_transfers(&mut current);

    let mut candidates = Vec::new();
    let max_rides = query.limits.max_transfers.saturating_add(1);
    for rides in 1..=max_rides {
        search.retain_current(&mut current);
        if current.is_empty() {
            break;
        }
        query.deadline.check()?;
        next.clear();
        for (pattern, first_board_index) in
            boardable_patterns(dense, &current, &query.allowed_transit_modes)
        {
            search.scan_pattern(pattern, first_board_index, &current, &mut next);
        }
        next.sort_stops();
        candidates.extend(search.egress_candidates(rides, &next, &egress_seconds_by_stop));
        search.relax_transfers(&mut next);
        std::mem::swap(&mut current, &mut next);
    }

    let mut stats = search.stats;
    let target_seconds = search.target_seconds;
    candidates.retain(|candidate| candidate.search_cost_seconds <= target_seconds);
    let selected_candidates = select(
        transit,
        candidates,
        query.limits.max_static_corridors.max(1),
        &mut stats,
    );
    let selected_corridors = build_corridors(transit, query, &selected_candidates)?;
    stats.corridors_retained = selected_corridors.len();
    stats.search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Ok((selected_corridors, stats))
}

fn egress_seconds_by_stop(
    dense: &DenseTopology,
    egress: &[EndpointStop],
) -> HashMap<StopIndex, u32> {
    let mut egress_seconds_by_stop = HashMap::<StopIndex, u32>::new();
    for egress in egress {
        let Some(stop) = dense.stop_index(&egress.stop_id) else {
            continue;
        };
        egress_seconds_by_stop
            .entry(stop)
            .and_modify(|walking| *walking = (*walking).min(egress.walking_seconds))
            .or_insert(egress.walking_seconds);
    }
    egress_seconds_by_stop
}

fn boardable_patterns(
    dense: &DenseTopology,
    current: &RoundLabels,
    allowed_modes: &[TransitMode],
) -> Vec<(PatternIndex, u32)> {
    let mut boardable_by_pattern = HashMap::<PatternIndex, u32>::new();
    for &stop in &current.touched {
        for position in dense.positions_at(stop) {
            if !allowed_modes.contains(&dense.pattern(position.pattern).mode) {
                continue;
            }
            let first_board_index = boardable_by_pattern
                .entry(position.pattern)
                .or_insert(position.stop_index);
            *first_board_index = (*first_board_index).min(position.stop_index);
        }
    }
    let mut boardable_patterns = boardable_by_pattern.into_iter().collect::<Vec<_>>();
    boardable_patterns.sort_unstable_by_key(|(pattern, _)| *pattern);
    boardable_patterns
}

fn build_corridors(
    transit: &TransitRuntime,
    query: &PlanningQuery,
    selected_candidates: &[Candidate],
) -> Result<Vec<Corridor>, RoutingError> {
    let equivalent_legs_by_key = equivalent_legs_for(transit, selected_candidates);
    let mut selected_corridors = Vec::with_capacity(selected_candidates.len());
    for (index, candidate) in selected_candidates.iter().enumerate() {
        tracing::debug!(
            target: "via_server::routing",
            corridor_rank = index + 1,
            theoretical_duration_seconds = candidate.theoretical_duration_seconds,
            pattern_sequence = ?candidate.pattern_sequence(),
            structural_steps = ?candidate.steps,
            cost_source = cost_source(candidate.transit_legs()).as_str(),
            "retained static corridor"
        );
        selected_corridors.push(
            Corridor::from_steps(
                transit,
                query,
                &candidate.access_stop_id,
                &candidate.steps,
                &candidate.egress_stop_id,
            )?
            .with_equivalent_legs(transit, &equivalent_legs_by_key),
        );
    }
    Ok(selected_corridors)
}

impl<'a> Search<'a> {
    fn new(transit: &'a TransitRuntime, limits: PlannerLimits) -> Self {
        let dense = transit.dense();
        Self {
            transit,
            dense,
            limits,
            nodes: Vec::new(),
            entries_by_stop: vec![Vec::new(); dense.stop_count()],
            target_seconds: limits.max_journey_duration_seconds,
            stats: CorridorSearchStats::default(),
        }
    }

    fn seed_access(&mut self, origin_access: &[EndpointStop]) -> RoundLabels {
        let mut current = RoundLabels::new(self.dense.stop_count());
        for access in origin_access {
            let Some(stop) = self.dense.stop_index(&access.stop_id) else {
                continue;
            };
            let node = self.nodes.len() as u32;
            self.nodes.push(Node {
                parent: None,
                access_stop: stop,
                step: None,
            });
            let structure = fingerprint(0, ORIGIN_TAG, u64::from(stop));
            if self.insert(stop, access.walking_seconds, structure, None) {
                current.push(
                    stop,
                    Label {
                        cost_seconds: access.walking_seconds,
                        penalty_seconds: 0,
                        last_mode: None,
                        node,
                        structure,
                        last_line: None,
                        after_ride: false,
                    },
                );
            }
        }
        current
    }

    fn egress_candidates(
        &mut self,
        rides: usize,
        next: &RoundLabels,
        egress_seconds_by_stop: &HashMap<StopIndex, u32>,
    ) -> Vec<Candidate> {
        let mut candidates = Vec::new();
        for &stop in &next.touched {
            let Some(egress_seconds) = egress_seconds_by_stop.get(&stop) else {
                continue;
            };
            for label in next.get(stop) {
                let duration = label
                    .cost_seconds
                    .saturating_sub(label.penalty_seconds)
                    .saturating_add(*egress_seconds);
                if duration > self.limits.max_journey_duration_seconds {
                    continue;
                }
                let search_cost_seconds = label.cost_seconds.saturating_add(*egress_seconds);
                self.target_seconds = self.target_seconds.min(
                    search_cost_seconds
                        .saturating_add(
                            search_cost_seconds.saturating_mul(TARGET_SLACK_PERCENT) / 100,
                        )
                        .saturating_add(TARGET_SLACK_SECONDS),
                );
                candidates.push(Candidate {
                    rides,
                    search_cost_seconds,
                    theoretical_duration_seconds: duration,
                    access_stop_id: self
                        .dense
                        .stop_id(self.nodes[label.node as usize].access_stop)
                        .clone(),
                    egress_stop_id: self.dense.stop_id(stop).clone(),
                    steps: self.steps(label.node),
                });
            }
        }
        candidates
    }
}

impl Search<'_> {
    fn scan_pattern(
        &mut self,
        pattern_index: PatternIndex,
        first_board_index: u32,
        current: &RoundLabels,
        next: &mut RoundLabels,
    ) {
        let dense = self.dense;
        let pattern = dense.pattern(pattern_index);
        let mode = pattern.mode;
        let line = pattern.line;
        let mut boarded = Vec::<Boarded>::new();
        for stop_index in first_board_index as usize..pattern.stops.len() {
            let stop = pattern.stops[stop_index];
            if stop_index > first_board_index as usize {
                let segment = pattern.segments[stop_index - 1];
                for entry in &mut boarded {
                    entry.ride_seconds = entry.ride_seconds.saturating_add(segment.seconds);
                    entry.cost_source.push_segment(segment.source);
                }
                let target_seconds = self.target_seconds;
                boarded.retain(|entry| {
                    entry.board_cost_seconds.saturating_add(entry.ride_seconds) <= target_seconds
                });
                for entry in &boarded {
                    let board_stop = pattern.stops[entry.board_index as usize];
                    let structure = fingerprint(
                        entry.label.structure,
                        ((mode as u64 + 1) << 40) | u64::from(board_stop),
                        u64::from(stop),
                    );
                    let cost_seconds = entry.board_cost_seconds.saturating_add(entry.ride_seconds);
                    if !self.insert(stop, cost_seconds, structure, Some(line)) {
                        continue;
                    }
                    let node = self.nodes.len() as u32;
                    let access_stop = self.nodes[entry.label.node as usize].access_stop;
                    self.nodes.push(Node {
                        parent: Some(entry.label.node),
                        access_stop,
                        step: Some(NodeStep::Transit {
                            pattern: pattern_index,
                            board_index: entry.board_index,
                            alight_index: stop_index as u32,
                            cost_source: entry.cost_source.finish(),
                        }),
                    });
                    self.stats.states_expanded += 1;
                    next.push(
                        stop,
                        Label {
                            cost_seconds,
                            penalty_seconds: entry.penalty_seconds,
                            last_mode: Some(mode),
                            node,
                            structure,
                            last_line: Some(line),
                            after_ride: true,
                        },
                    );
                }
            }
            let labels = current.get(stop);
            if labels.is_empty() {
                continue;
            }
            if stop_index + 1 >= pattern.stops.len() {
                continue;
            }
            let already_boarded = boarded.len();
            for label in labels {
                if label.last_line == Some(line) {
                    continue;
                }
                let transfer_penalty_seconds = label.last_mode.map_or(0, |last_mode| {
                    self.limits
                        .transfer_penalty_seconds(TransferKind::between(Some(last_mode), mode))
                });
                let board_cost_seconds = label
                    .cost_seconds
                    .saturating_add(if label.after_ride {
                        self.transit.same_stop_transfer_seconds()
                    } else {
                        0
                    })
                    .saturating_add(transfer_penalty_seconds);
                boarded.push(Boarded {
                    label: *label,
                    board_index: stop_index as u32,
                    board_cost_seconds,
                    penalty_seconds: label
                        .penalty_seconds
                        .saturating_add(transfer_penalty_seconds),
                    ride_seconds: 0,
                    cost_source: CostSourceTracker::default(),
                });
            }
            if boarded.len() == already_boarded {
                continue;
            }
            boarded
                .sort_by_key(|entry| entry.board_cost_seconds.saturating_add(entry.ride_seconds));
            let mut index = 0;
            while index < boarded.len() {
                let structure = boarded[index].label.structure;
                if boarded[..index]
                    .iter()
                    .any(|entry| entry.label.structure == structure)
                {
                    boarded.remove(index);
                } else {
                    index += 1;
                }
            }
            boarded.truncate(LABELS_PER_STOP);
        }
    }

    fn relax_transfers(&mut self, labels: &mut RoundLabels) {
        let dense = self.dense;
        let mut walked = Vec::<(StopIndex, Label)>::new();
        labels.sort_stops();
        for &stop in &labels.touched {
            for label in labels.get(stop) {
                if !label.after_ride && self.nodes[label.node as usize].parent.is_some() {
                    continue;
                }
                for shortcut in dense.transfers_from(stop) {
                    let cost_seconds = label.cost_seconds.saturating_add(shortcut.duration_seconds);
                    let structure = fingerprint(
                        label.structure,
                        TRANSFER_TAG | u64::from(stop),
                        u64::from(shortcut.to),
                    );
                    if !self.insert(shortcut.to, cost_seconds, structure, label.last_line) {
                        continue;
                    }
                    let node = self.nodes.len() as u32;
                    let access_stop = self.nodes[label.node as usize].access_stop;
                    self.nodes.push(Node {
                        parent: Some(label.node),
                        access_stop,
                        step: Some(NodeStep::Transfer {
                            edge: shortcut.edge,
                        }),
                    });
                    walked.push((
                        shortcut.to,
                        Label {
                            cost_seconds,
                            penalty_seconds: label.penalty_seconds,
                            last_mode: label.last_mode,
                            node,
                            structure,
                            last_line: label.last_line,
                            after_ride: false,
                        },
                    ));
                }
            }
        }
        for (stop, label) in walked {
            labels.push(stop, label);
        }
    }

    fn insert(
        &mut self,
        stop: StopIndex,
        cost_seconds: u32,
        structure: u64,
        last_line: Option<u64>,
    ) -> bool {
        if cost_seconds > self.target_seconds {
            return false;
        }
        let entries = &mut self.entries_by_stop[stop as usize];
        if let Some(index) = entries
            .iter()
            .position(|known| known.structure == structure && known.last_line == last_line)
        {
            if entries[index].cost_seconds <= cost_seconds {
                return false;
            }
            entries.remove(index);
        }
        let mut same_structure = 0;
        let mut better_structures = 0;
        for (index, known) in entries.iter().enumerate() {
            if known.cost_seconds > cost_seconds {
                break;
            }
            if known.structure == structure {
                same_structure += 1;
            } else if !entries[..index]
                .iter()
                .any(|earlier| earlier.structure == known.structure)
            {
                better_structures += 1;
            }
        }
        if same_structure >= ROUTE_VARIANTS_PER_STRUCTURE || better_structures >= LABELS_PER_STOP {
            return false;
        }
        let position = entries.partition_point(|known| known.cost_seconds <= cost_seconds);
        entries.insert(
            position,
            BagEntry {
                cost_seconds,
                structure,
                last_line,
            },
        );
        let mut structures = 0;
        let mut index = 0;
        while index < entries.len() {
            let structure = entries[index].structure;
            let variants = entries[..index]
                .iter()
                .filter(|known| known.structure == structure)
                .count();
            let keep = if variants == 0 {
                structures < LABELS_PER_STOP
            } else {
                variants < ROUTE_VARIANTS_PER_STRUCTURE
            };
            if keep {
                structures += usize::from(variants == 0);
                index += 1;
            } else {
                entries.remove(index);
            }
        }
        true
    }

    fn retain_current(&self, current: &mut RoundLabels) {
        let RoundLabels {
            labels_by_stop,
            touched,
        } = current;
        touched.retain(|stop| {
            let entries = &self.entries_by_stop[*stop as usize];
            let labels = &mut labels_by_stop[*stop as usize];
            labels.retain(|label| {
                entries.iter().any(|known| {
                    known.cost_seconds == label.cost_seconds
                        && known.structure == label.structure
                        && known.last_line == label.last_line
                })
            });
            !labels.is_empty()
        });
    }

    fn steps(&self, node: u32) -> Vec<StaticCorridorStep> {
        let dense = self.dense;
        let mut steps = Vec::new();
        let mut cursor = Some(node);
        while let Some(index) = cursor {
            let node = &self.nodes[index as usize];
            match node.step {
                Some(NodeStep::Transit {
                    pattern,
                    board_index,
                    alight_index,
                    cost_source,
                }) => {
                    let stops = &dense.pattern(pattern).stops;
                    steps.push(StaticCorridorStep::Transit(StaticTransitLeg {
                        pattern_id: dense.pattern_id(pattern).clone(),
                        board_stop_id: dense.stop_id(stops[board_index as usize]).clone(),
                        board_index: board_index as usize,
                        alight_stop_id: dense.stop_id(stops[alight_index as usize]).clone(),
                        alight_index: alight_index as usize,
                        cost_source,
                    }));
                }
                Some(NodeStep::Transfer { edge }) => {
                    let edge = &self.transit.network().transfer_edges[edge as usize];
                    steps.push(StaticCorridorStep::SurfaceTransfer {
                        from: edge.from.clone(),
                        to: edge.to.clone(),
                        walking_duration_seconds: edge.duration_seconds,
                        walking_distance_metres: edge.distance_metres,
                    });
                }
                None => {}
            }
            cursor = node.parent;
        }
        steps.reverse();
        steps
    }
}

fn mix(value: u64) -> u64 {
    let mut value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn fingerprint(parent: u64, first: u64, second: u64) -> u64 {
    mix(parent ^ mix(first ^ mix(second)))
}

fn select(
    transit: &TransitRuntime,
    candidates: Vec<Candidate>,
    limit: usize,
    stats: &mut CorridorSearchStats,
) -> Vec<Candidate> {
    let mut candidate_by_signature = HashMap::<EquivalentCorridorSignature, Candidate>::new();
    for candidate in candidates {
        let structure = EquivalentCorridorSignature {
            access_stop_id: candidate.access_stop_id.clone(),
            steps: equivalent_steps(transit, &candidate.steps),
            egress_stop_id: candidate.egress_stop_id.clone(),
        };
        let replace = candidate_by_signature.get(&structure).is_none_or(|known| {
            candidate
                .search_cost_seconds
                .cmp(&known.search_cost_seconds)
                .then_with(|| corridor_order_key(&candidate).cmp(&corridor_order_key(known)))
                .is_lt()
        });
        if replace {
            candidate_by_signature.insert(structure, candidate);
        }
    }
    stats.unique_corridors_found = candidate_by_signature.len();
    let mut candidates = candidate_by_signature.into_values().collect::<Vec<_>>();
    candidates.sort_by_cached_key(|candidate| {
        let (access, egress, steps) = corridor_order_key(candidate);
        (
            candidate.search_cost_seconds,
            candidate.rides,
            access.to_owned(),
            egress.to_owned(),
            steps,
        )
    });
    let mut chosen_indices = HashSet::new();
    let mut best_with_fewer_rides = u32::MAX;
    let max_rides = candidates
        .iter()
        .map(|candidate| candidate.rides)
        .max()
        .unwrap_or(0);
    for rides in 1..=max_rides {
        if let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.rides == rides)
        {
            let duration = candidates[index].search_cost_seconds;
            if duration < best_with_fewer_rides {
                chosen_indices.insert(index);
                best_with_fewer_rides = duration;
            }
        }
    }
    let mut mode_sets = chosen_indices
        .iter()
        .map(|&index| corridor_modes(transit, &candidates[index]))
        .collect::<HashSet<_>>();
    for (index, candidate) in candidates.iter().enumerate() {
        if chosen_indices.len() >= limit {
            break;
        }
        if mode_sets.insert(corridor_modes(transit, candidate)) {
            chosen_indices.insert(index);
        }
    }
    let mut sequences = chosen_indices
        .iter()
        .map(|&index| candidates[index].pattern_sequence())
        .collect::<HashSet<_>>();
    for (index, candidate) in candidates.iter().enumerate() {
        if chosen_indices.len() >= limit {
            break;
        }
        if sequences.insert(candidate.pattern_sequence()) {
            chosen_indices.insert(index);
        }
    }
    for index in 0..candidates.len() {
        if chosen_indices.len() >= limit {
            break;
        }
        chosen_indices.insert(index);
    }
    candidates
        .into_iter()
        .enumerate()
        .filter(|(index, _)| chosen_indices.contains(index))
        .map(|(_, candidate)| candidate)
        .collect()
}

fn equivalent_legs_for(
    transit: &TransitRuntime,
    candidates: &[Candidate],
) -> HashMap<EquivalentLegKey, Vec<StaticTransitLeg>> {
    let mut equivalent_legs_by_key = HashMap::<EquivalentLegKey, Vec<StaticTransitLeg>>::new();
    for leg in candidates.iter().flat_map(Candidate::transit_legs) {
        let key = equivalent_leg_key(transit, leg);
        if equivalent_legs_by_key.contains_key(&key) {
            continue;
        }
        let mut legs = Vec::new();
        for position in transit.pattern_positions_at(&leg.board_stop_id) {
            // Patterns the dense topology dropped (never timeable) are not alternatives.
            if transit
                .dense()
                .pattern_index(&position.pattern_id)
                .is_none()
            {
                continue;
            }
            let Some((pattern, _)) = pattern_with_mode(transit, &position.pattern_id, &[key.mode])
            else {
                continue;
            };
            let Some(offset) = pattern.stops[position.stop_index + 1..]
                .iter()
                .position(|stop_id| *stop_id == leg.alight_stop_id)
            else {
                continue;
            };
            let alight_index = position.stop_index + 1 + offset;
            let mut cost_source = CostSourceTracker::default();
            for segment in &pattern.static_profile.segments[position.stop_index..alight_index] {
                cost_source.push_segment(segment.source);
            }
            legs.push(StaticTransitLeg {
                pattern_id: pattern.id.clone(),
                board_stop_id: leg.board_stop_id.clone(),
                board_index: position.stop_index,
                alight_stop_id: leg.alight_stop_id.clone(),
                alight_index,
                cost_source: cost_source.finish(),
            });
        }
        if !legs.iter().any(|known| known == leg) {
            legs.push(leg.clone());
        }
        equivalent_legs_by_key.insert(key, legs);
    }
    equivalent_legs_by_key
}

fn pattern_with_mode<'a>(
    transit: &'a TransitRuntime,
    pattern_id: &TransitPatternId,
    allowed_modes: &[TransitMode],
) -> Option<(&'a TransitPattern, TransitMode)> {
    let pattern = transit.pattern(pattern_id)?;
    let mode = transit.pattern_mode(pattern_id);
    allowed_modes.contains(&mode).then_some((pattern, mode))
}

fn corridor_modes(transit: &TransitRuntime, candidate: &Candidate) -> Vec<TransitMode> {
    let mut modes = candidate
        .transit_legs()
        .map(|leg| transit.pattern_mode(&leg.pattern_id))
        .collect::<Vec<_>>();
    modes.sort_unstable();
    modes.dedup();
    modes
}

fn corridor_order_key(candidate: &Candidate) -> (&str, &str, String) {
    (
        candidate.access_stop_id.as_str(),
        candidate.egress_stop_id.as_str(),
        format!("{:?}", candidate.steps),
    )
}

pub(super) fn cost_source<'a>(
    legs: impl Iterator<Item = &'a StaticTransitLeg>,
) -> StaticCostSource {
    let mut cost_source = CostSourceTracker::default();
    for leg in legs {
        cost_source.push(leg.cost_source);
    }
    cost_source.finish()
}
