use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    time::Instant,
};

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId, TransitRouteId},
    transit::network::{SegmentTimeSource, TransitMode, TransitPattern},
    transit::runtime::TransitRuntime,
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

#[derive(Clone, PartialEq, Eq)]
enum LineKey {
    Route(TransitRouteId),
    Pattern(TransitPatternId),
}

#[derive(Clone)]
struct Label {
    cost_seconds: u32,
    penalty_seconds: u32,
    last_mode: Option<TransitMode>,
    node: usize,
    structure: u64,
    last_line: Option<LineKey>,
    after_ride: bool,
}

struct Node {
    parent: Option<usize>,
    access_stop_id: BoardingPointId,
    step: Option<StaticCorridorStep>,
}

struct Boarded {
    label: Label,
    board_index: usize,
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

struct Search<'a> {
    transit: &'a TransitRuntime,
    limits: PlannerLimits,
    nodes: Vec<Node>,
    entries_by_stop: HashMap<BoardingPointId, Vec<BagEntry>>,
    target_seconds: u32,
    stats: CorridorSearchStats,
}

type RoundLabels = HashMap<BoardingPointId, Vec<Label>>;

#[derive(Clone, PartialEq, Eq)]
struct BagEntry {
    cost_seconds: u32,
    structure: u64,
    last_line: Option<LineKey>,
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
    let egress_seconds_by_stop = egress_seconds_by_stop(&query.destination_egress);
    let mut search = Search::new(transit, query.limits);
    let mut current = search.seed_access(&query.origin_access);
    search.relax_transfers(&mut current);

    let mut candidates = Vec::new();
    let max_rides = query.limits.max_transfers.saturating_add(1);
    for rides in 1..=max_rides {
        search.retain_current(&mut current);
        if current.is_empty() {
            break;
        }
        query.deadline.check()?;
        let mut next = RoundLabels::new();
        for (pattern, mode, first_board_index) in
            boardable_patterns(transit, &current, &query.allowed_transit_modes)
        {
            search.scan_pattern(pattern, mode, first_board_index, &current, &mut next);
        }
        candidates.extend(search.egress_candidates(rides, &next, &egress_seconds_by_stop));
        search.relax_transfers(&mut next);
        current = next;
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

fn egress_seconds_by_stop(egress: &[EndpointStop]) -> HashMap<&BoardingPointId, u32> {
    let mut egress_seconds_by_stop = HashMap::<&BoardingPointId, u32>::new();
    for egress in egress {
        egress_seconds_by_stop
            .entry(&egress.stop_id)
            .and_modify(|walking| *walking = (*walking).min(egress.walking_seconds))
            .or_insert(egress.walking_seconds);
    }
    egress_seconds_by_stop
}

fn boardable_patterns<'a>(
    transit: &'a TransitRuntime,
    current: &RoundLabels,
    allowed_modes: &[TransitMode],
) -> Vec<(&'a TransitPattern, TransitMode, usize)> {
    let mut boardable_by_pattern =
        HashMap::<&TransitPatternId, (&TransitPattern, TransitMode, usize)>::new();
    for stop_id in current.keys() {
        for position in transit.pattern_positions_at(stop_id) {
            let Some((pattern, mode)) =
                pattern_with_mode(transit, &position.pattern_id, allowed_modes)
            else {
                continue;
            };
            let (_, _, first_board_index) = boardable_by_pattern
                .entry(&position.pattern_id)
                .or_insert((pattern, mode, position.stop_index));
            *first_board_index = (*first_board_index).min(position.stop_index);
        }
    }
    let mut boardable_patterns = boardable_by_pattern.into_values().collect::<Vec<_>>();
    boardable_patterns.sort_unstable_by(|left, right| left.0.id.as_str().cmp(right.0.id.as_str()));
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
        Self {
            transit,
            limits,
            nodes: Vec::new(),
            entries_by_stop: HashMap::new(),
            target_seconds: limits.max_journey_duration_seconds,
            stats: CorridorSearchStats::default(),
        }
    }

    fn seed_access(&mut self, origin_access: &[EndpointStop]) -> RoundLabels {
        let mut current = RoundLabels::new();
        for access in origin_access {
            let node = self.nodes.len();
            self.nodes.push(Node {
                parent: None,
                access_stop_id: access.stop_id.clone(),
                step: None,
            });
            let structure = fingerprint(0, &access.stop_id);
            if self.insert(&access.stop_id, access.walking_seconds, structure, None) {
                current
                    .entry(access.stop_id.clone())
                    .or_default()
                    .push(Label {
                        cost_seconds: access.walking_seconds,
                        penalty_seconds: 0,
                        last_mode: None,
                        node,
                        structure,
                        last_line: None,
                        after_ride: false,
                    });
            }
        }
        current
    }

    fn egress_candidates(
        &mut self,
        rides: usize,
        next: &RoundLabels,
        egress_seconds_by_stop: &HashMap<&BoardingPointId, u32>,
    ) -> Vec<Candidate> {
        let mut candidates = Vec::new();
        for stop_id in sorted_stops(next) {
            let Some(egress_seconds) = egress_seconds_by_stop.get(stop_id) else {
                continue;
            };
            for label in &next[stop_id] {
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
                    access_stop_id: self.nodes[label.node].access_stop_id.clone(),
                    egress_stop_id: stop_id.clone(),
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
        pattern: &TransitPattern,
        mode: TransitMode,
        first_board_index: usize,
        current: &RoundLabels,
        next: &mut RoundLabels,
    ) {
        let line = match mode {
            TransitMode::Rail | TransitMode::HighSpeedRail => LineKey::Pattern(pattern.id.clone()),
            TransitMode::Bus | TransitMode::Metro => LineKey::Route(pattern.route_id.clone()),
        };
        let segments = &pattern.static_profile.segments;
        let mut boarded = Vec::<Boarded>::new();
        for stop_index in first_board_index..pattern.stops.len() {
            let stop_id = &pattern.stops[stop_index];
            if stop_index > first_board_index {
                let segment = &segments[stop_index - 1];
                for entry in &mut boarded {
                    entry.ride_seconds = entry.ride_seconds.saturating_add(segment.seconds);
                    entry.cost_source.push_segment(segment.source);
                }
                boarded.retain(|entry| {
                    entry.board_cost_seconds.saturating_add(entry.ride_seconds)
                        <= self.target_seconds
                });
                for entry in &boarded {
                    let board_stop_id = &pattern.stops[entry.board_index];
                    let structure =
                        fingerprint(entry.label.structure, &(mode as u8, board_stop_id, stop_id));
                    let cost_seconds = entry.board_cost_seconds.saturating_add(entry.ride_seconds);
                    if !self.insert(stop_id, cost_seconds, structure, Some(&line)) {
                        continue;
                    }
                    let node = self.nodes.len();
                    self.nodes.push(Node {
                        parent: Some(entry.label.node),
                        access_stop_id: self.nodes[entry.label.node].access_stop_id.clone(),
                        step: Some(StaticCorridorStep::Transit(StaticTransitLeg {
                            pattern_id: pattern.id.clone(),
                            board_stop_id: board_stop_id.clone(),
                            board_index: entry.board_index,
                            alight_stop_id: stop_id.clone(),
                            alight_index: stop_index,
                            cost_source: entry.cost_source.finish(),
                        })),
                    });
                    self.stats.states_expanded += 1;
                    next.entry(stop_id.clone()).or_default().push(Label {
                        cost_seconds,
                        penalty_seconds: entry.penalty_seconds,
                        last_mode: Some(mode),
                        node,
                        structure,
                        last_line: Some(line.clone()),
                        after_ride: true,
                    });
                }
            }
            let Some(labels) = current.get(stop_id) else {
                continue;
            };
            if stop_index + 1 >= pattern.stops.len() {
                continue;
            }
            let already_boarded = boarded.len();
            for label in labels {
                if label.last_line.as_ref() == Some(&line) {
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
                    label: label.clone(),
                    board_index: stop_index,
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
        let mut walked = RoundLabels::new();
        for stop_id in sorted_stops(labels) {
            for label in &labels[stop_id] {
                if !label.after_ride && self.nodes[label.node].parent.is_some() {
                    continue;
                }
                for shortcut in self.transit.transfers_from(stop_id) {
                    let cost_seconds = label.cost_seconds.saturating_add(shortcut.duration_seconds);
                    let structure = fingerprint(label.structure, &(&shortcut.from, &shortcut.to));
                    if !self.insert(
                        &shortcut.to,
                        cost_seconds,
                        structure,
                        label.last_line.as_ref(),
                    ) {
                        continue;
                    }
                    let node = self.nodes.len();
                    self.nodes.push(Node {
                        parent: Some(label.node),
                        access_stop_id: self.nodes[label.node].access_stop_id.clone(),
                        step: Some(StaticCorridorStep::SurfaceTransfer {
                            from: shortcut.from.clone(),
                            to: shortcut.to.clone(),
                            walking_duration_seconds: shortcut.duration_seconds,
                            walking_distance_metres: shortcut.distance_metres,
                        }),
                    });
                    walked.entry(shortcut.to.clone()).or_default().push(Label {
                        cost_seconds,
                        penalty_seconds: label.penalty_seconds,
                        last_mode: label.last_mode,
                        node,
                        structure,
                        last_line: label.last_line.clone(),
                        after_ride: false,
                    });
                }
            }
        }
        for (stop_id, stop_labels) in walked {
            labels.entry(stop_id).or_default().extend(stop_labels);
        }
    }

    fn insert(
        &mut self,
        stop_id: &BoardingPointId,
        cost_seconds: u32,
        structure: u64,
        last_line: Option<&LineKey>,
    ) -> bool {
        if cost_seconds > self.target_seconds {
            return false;
        }
        let entries = self.entries_by_stop.entry(stop_id.clone()).or_default();
        if let Some(index) = entries
            .iter()
            .position(|known| known.structure == structure && known.last_line.as_ref() == last_line)
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
                last_line: last_line.cloned(),
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
        current.retain(|stop_id, labels| {
            let Some(entries) = self.entries_by_stop.get(stop_id) else {
                return false;
            };
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

    fn steps(&self, node: usize) -> Vec<StaticCorridorStep> {
        let mut steps = Vec::new();
        let mut cursor = Some(node);
        while let Some(index) = cursor {
            if let Some(step) = &self.nodes[index].step {
                steps.push(step.clone());
            }
            cursor = self.nodes[index].parent;
        }
        steps.reverse();
        steps
    }
}

fn sorted_stops(labels: &RoundLabels) -> Vec<&BoardingPointId> {
    let mut stops = labels.keys().collect::<Vec<_>>();
    stops.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    stops
}

fn fingerprint(parent: u64, value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    parent.hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
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
