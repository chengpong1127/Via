use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    time::Instant,
};

use crate::domain::{
    ids::{BoardingPointId, TransitRouteId},
    transit::{SegmentTimeSource, TransitMode},
    transit_runtime::TransitRuntime,
};

use crate::routing::model::PlanningQuery;

use super::{
    pattern_index::CorridorPatternIndex,
    retention::RetainedCorridorSet,
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
    types::{
        CorridorSearchStats, EquivalentCorridorSignature, EquivalentLegKey, StaticCorridor,
        StaticCorridorStep, StaticCostSource, StaticTransitLeg, equivalent_leg_key,
        equivalent_steps,
    },
};

const LABELS_PER_STOP: usize = 4;
const ROUTE_VARIANTS_PER_STRUCTURE: usize = 2;
const TARGET_SLACK_PERCENT: u32 = 50;
const TARGET_SLACK_SECONDS: u32 = 600;

#[derive(Clone)]
struct Label {
    cost_seconds: u32,
    penalty_seconds: u32,
    last_mode: Option<TransitMode>,
    node: usize,
    structure: u64,
    last_line_id: Option<TransitRouteId>,
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
    first_source: Option<SegmentTimeSource>,
    mixed_source: bool,
}

struct Candidate {
    rides: usize,
    search_cost_seconds: u32,
    corridor: StaticCorridor,
}

struct Search<'a> {
    network: &'a TransitRuntime,
    transfers: &'a TransferShortcutIndex,
    nodes: Vec<Node>,
    best: HashMap<BoardingPointId, Vec<BagEntry>>,
    max_seconds: u32,
    target_seconds: u32,
    metro_transfer_penalty_seconds: u32,
    bus_transfer_penalty_seconds: u32,
    stats: CorridorSearchStats,
}

type RoundLabels = HashMap<BoardingPointId, Vec<Label>>;

#[derive(Clone, PartialEq, Eq)]
struct BagEntry {
    cost_seconds: u32,
    structure: u64,
    last_line_id: Option<TransitRouteId>,
}

pub fn generate(
    network: &TransitRuntime,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &PlanningQuery,
) -> (RetainedCorridorSet, CorridorSearchStats) {
    let started = Instant::now();
    let mut stats = CorridorSearchStats::default();
    let patterns = CorridorPatternIndex::new(
        network,
        &query.allowed_transit_modes,
        &mut stats,
        query.deadline,
    );
    let egress_by_stop = query.destination_egress.iter().fold(
        HashMap::<BoardingPointId, u32>::new(),
        |mut by_stop, egress| {
            let walking = by_stop.entry(egress.stop_id.clone()).or_insert(u32::MAX);
            *walking = (*walking).min(egress.walking_seconds);
            by_stop
        },
    );
    let mut search = Search {
        network,
        transfers,
        nodes: Vec::new(),
        best: HashMap::new(),
        max_seconds: query.limits.max_journey_duration_seconds,
        target_seconds: query.limits.max_journey_duration_seconds,
        metro_transfer_penalty_seconds: query.limits.metro_transfer_penalty_seconds,
        bus_transfer_penalty_seconds: query.limits.bus_transfer_penalty_seconds,
        stats,
    };
    let mut current = RoundLabels::new();
    for access in &query.origin_access {
        let node = search.nodes.len();
        search.nodes.push(Node {
            parent: None,
            access_stop_id: access.stop_id.clone(),
            step: None,
        });
        let label = Label {
            cost_seconds: access.walking_seconds,
            penalty_seconds: 0,
            last_mode: None,
            node,
            structure: fingerprint(0, &access.stop_id),
            last_line_id: None,
            after_ride: false,
        };
        if search.insert(&access.stop_id, &label) {
            current
                .entry(access.stop_id.clone())
                .or_default()
                .push(label);
        }
    }
    search.relax_transfers(&mut current);

    let mut candidates = Vec::new();
    let max_rides = query.limits.max_transfers.saturating_add(1);
    for rides in 1..=max_rides {
        search.retain_current(&mut current);
        if current.is_empty() {
            break;
        }
        if query.deadline.is_elapsed() {
            search.stats.deadline_exceeded = true;
            return (RetainedCorridorSet::default(), search.stats);
        }
        let mut routes = HashMap::<usize, usize>::new();
        for stop_id in current.keys() {
            for position in topology.occurrences_at(stop_id) {
                if let Some(group_index) = patterns.group_index(&position.pattern_id) {
                    let first = routes.entry(group_index).or_insert(position.stop_index);
                    *first = (*first).min(position.stop_index);
                }
            }
        }
        let mut routes = routes.into_iter().collect::<Vec<_>>();
        routes.sort_unstable();
        let mut next = RoundLabels::new();
        for (group_index, first_index) in routes {
            search.scan_route(&patterns, group_index, first_index, &current, &mut next);
        }
        for stop_id in sorted_stops(&next) {
            let Some(egress_seconds) = egress_by_stop.get(stop_id) else {
                continue;
            };
            for label in &next[stop_id] {
                let duration = label
                    .cost_seconds
                    .saturating_sub(label.penalty_seconds)
                    .saturating_add(*egress_seconds);
                if duration > search.max_seconds {
                    continue;
                }
                let search_cost_seconds = label.cost_seconds.saturating_add(*egress_seconds);
                search.target_seconds = search.target_seconds.min(
                    search_cost_seconds
                        .saturating_add(
                            search_cost_seconds.saturating_mul(TARGET_SLACK_PERCENT) / 100,
                        )
                        .saturating_add(TARGET_SLACK_SECONDS),
                );
                let steps = search.steps(label.node);
                candidates.push(Candidate {
                    rides,
                    search_cost_seconds,
                    corridor: StaticCorridor {
                        access_stop_id: search.nodes[label.node].access_stop_id.clone(),
                        egress_stop_id: stop_id.clone(),
                        theoretical_duration_seconds: duration,
                        cost_source: cost_source(&steps),
                        steps,
                    },
                });
            }
        }
        search.relax_transfers(&mut next);
        current = next;
    }

    let mut stats = search.stats;
    let target_seconds = search.target_seconds;
    candidates.retain(|candidate| candidate.search_cost_seconds <= target_seconds);
    let selected = select(
        network,
        candidates,
        query.limits.max_static_corridors.max(1),
        &mut stats,
    );
    let equivalent_legs = equivalent_legs_for(network, topology, &patterns, &selected);
    let (retained, pattern_count, bus_stop_count) =
        RetainedCorridorSet::from_retained(network, &equivalent_legs, selected);
    stats.corridors_retained = retained.retained().len();
    stats.corridor_patterns = pattern_count;
    stats.corridor_positions = retained.timing_positions().count();
    stats.corridor_bus_stops = bus_stop_count;
    stats.search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    (retained, stats)
}

impl Search<'_> {
    fn scan_route(
        &mut self,
        patterns: &CorridorPatternIndex,
        group_index: usize,
        first_index: usize,
        current: &RoundLabels,
        next: &mut RoundLabels,
    ) {
        let group = patterns.group(group_index);
        let line_id = match group.mode {
            TransitMode::Rail | TransitMode::HighSpeedRail => {
                TransitRouteId::new(group.pattern_id.as_str())
            }
            TransitMode::Bus | TransitMode::Metro => self
                .network
                .required_pattern(&group.pattern_id)
                .route_id
                .clone(),
        };
        let mut boarded = Vec::<Boarded>::new();
        for stop_index in first_index..group.stops.len() {
            let stop_id = &group.stops[stop_index];
            if stop_index > first_index {
                let segment = &group.segments[stop_index - 1];
                for entry in &mut boarded {
                    entry.ride_seconds = entry.ride_seconds.saturating_add(segment.seconds);
                    entry.mixed_source |= entry
                        .first_source
                        .is_some_and(|first| first != segment.source);
                    entry.first_source.get_or_insert(segment.source);
                }
                boarded.retain(|entry| {
                    entry.board_cost_seconds.saturating_add(entry.ride_seconds)
                        <= self.target_seconds.min(self.max_seconds)
                });
                for entry in &boarded {
                    let leg = StaticTransitLeg {
                        pattern_id: group.pattern_id.clone(),
                        board_stop_id: group.stops[entry.board_index].clone(),
                        board_index: entry.board_index,
                        alight_stop_id: stop_id.clone(),
                        alight_index: stop_index,
                        cost_source: leg_cost_source(entry.first_source, entry.mixed_source),
                    };
                    let structure = fingerprint(
                        entry.label.structure,
                        &(group.mode as u8, &leg.board_stop_id, stop_id),
                    );
                    let node = self.nodes.len();
                    let label = Label {
                        cost_seconds: entry.board_cost_seconds.saturating_add(entry.ride_seconds),
                        penalty_seconds: entry.penalty_seconds,
                        last_mode: Some(group.mode),
                        node,
                        structure,
                        last_line_id: Some(line_id.clone()),
                        after_ride: true,
                    };
                    if self.insert(stop_id, &label) {
                        self.nodes.push(Node {
                            parent: Some(entry.label.node),
                            access_stop_id: self.nodes[entry.label.node].access_stop_id.clone(),
                            step: Some(StaticCorridorStep::Transit(leg)),
                        });
                        self.stats.states_expanded += 1;
                        next.entry(stop_id.clone()).or_default().push(label);
                    }
                }
            }
            let Some(labels) = current.get(stop_id) else {
                continue;
            };
            if stop_index + 1 >= group.stops.len() {
                continue;
            }
            for label in labels {
                if label.last_line_id.as_ref() == Some(&line_id) {
                    continue;
                }
                let transfer_penalty_seconds = match label.last_mode {
                    None => 0,
                    Some(TransitMode::Metro) if group.mode == TransitMode::Metro => {
                        self.metro_transfer_penalty_seconds
                    }
                    Some(_) => self.bus_transfer_penalty_seconds,
                };
                let board_cost_seconds = label
                    .cost_seconds
                    .saturating_add(if label.after_ride {
                        self.network.same_stop_transfer_seconds()
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
                    first_source: None,
                    mixed_source: false,
                });
            }
            boarded
                .sort_by_key(|entry| entry.board_cost_seconds.saturating_add(entry.ride_seconds));
            let mut seen = HashSet::new();
            boarded.retain(|entry| seen.insert(entry.label.structure));
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
                for shortcut in self.transfers.transfers_from(stop_id) {
                    let cost_seconds = label.cost_seconds.saturating_add(shortcut.duration_seconds);
                    if cost_seconds > self.target_seconds.min(self.max_seconds) {
                        continue;
                    }
                    let node = self.nodes.len();
                    let transferred = Label {
                        cost_seconds,
                        penalty_seconds: label.penalty_seconds,
                        last_mode: label.last_mode,
                        node,
                        structure: fingerprint(label.structure, &(&shortcut.from, &shortcut.to)),
                        last_line_id: label.last_line_id.clone(),
                        after_ride: false,
                    };
                    if self.insert(&shortcut.to, &transferred) {
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
                        walked
                            .entry(shortcut.to.clone())
                            .or_default()
                            .push(transferred);
                    }
                }
            }
        }
        for (stop_id, stop_labels) in walked {
            labels.entry(stop_id).or_default().extend(stop_labels);
        }
    }

    fn insert(&mut self, stop_id: &BoardingPointId, label: &Label) -> bool {
        if label.cost_seconds > self.target_seconds.min(self.max_seconds) {
            return false;
        }
        let bag = self.best.entry(stop_id.clone()).or_default();
        if let Some(index) = bag.iter().position(|known| {
            known.structure == label.structure && known.last_line_id == label.last_line_id
        }) {
            if bag[index].cost_seconds <= label.cost_seconds {
                return false;
            }
            bag.remove(index);
        }
        if bag
            .iter()
            .filter(|known| {
                known.structure == label.structure && known.cost_seconds <= label.cost_seconds
            })
            .count()
            >= ROUTE_VARIANTS_PER_STRUCTURE
        {
            return false;
        }
        let better_structures = bag
            .iter()
            .filter(|known| {
                known.structure != label.structure && known.cost_seconds <= label.cost_seconds
            })
            .map(|known| known.structure)
            .collect::<HashSet<_>>();
        if better_structures.len() >= LABELS_PER_STOP {
            return false;
        }
        bag.push(BagEntry {
            cost_seconds: label.cost_seconds,
            structure: label.structure,
            last_line_id: label.last_line_id.clone(),
        });
        bag.sort_by_key(|known| known.cost_seconds);
        let mut structures = Vec::new();
        let mut variants = HashMap::<u64, usize>::new();
        bag.retain(|known| {
            let count = variants.entry(known.structure).or_default();
            *count += 1;
            if !structures.contains(&known.structure) {
                structures.push(known.structure);
            }
            *count <= ROUTE_VARIANTS_PER_STRUCTURE
                && structures
                    .iter()
                    .position(|structure| *structure == known.structure)
                    .is_some_and(|rank| rank < LABELS_PER_STOP)
        });
        true
    }

    fn retain_current(&self, current: &mut RoundLabels) {
        current.retain(|stop_id, labels| {
            let Some(bag) = self.best.get(stop_id) else {
                return false;
            };
            labels.retain(|label| {
                bag.iter().any(|known| {
                    known.cost_seconds == label.cost_seconds
                        && known.structure == label.structure
                        && known.last_line_id == label.last_line_id
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

fn leg_cost_source(first: Option<SegmentTimeSource>, mixed: bool) -> StaticCostSource {
    if mixed {
        return StaticCostSource::Mixed;
    }
    match first {
        Some(SegmentTimeSource::Schedule) => StaticCostSource::ExactSchedule,
        Some(SegmentTimeSource::Derived) => StaticCostSource::DerivedBus,
        Some(SegmentTimeSource::Estimated) | None => StaticCostSource::OptimisticEstimate,
    }
}

fn select(
    network: &TransitRuntime,
    candidates: Vec<Candidate>,
    limit: usize,
    stats: &mut CorridorSearchStats,
) -> Vec<StaticCorridor> {
    let mut unique = HashMap::<EquivalentCorridorSignature, Candidate>::new();
    for candidate in candidates {
        let structure = EquivalentCorridorSignature {
            access_stop_id: candidate.corridor.access_stop_id.clone(),
            steps: equivalent_steps(network, &candidate.corridor.steps),
            egress_stop_id: candidate.corridor.egress_stop_id.clone(),
        };
        let replace = unique.get(&structure).is_none_or(|known| {
            (
                candidate.search_cost_seconds,
                corridor_order_key(&candidate.corridor),
            ) < (
                known.search_cost_seconds,
                corridor_order_key(&known.corridor),
            )
        });
        if replace {
            unique.insert(structure, candidate);
        }
    }
    stats.unique_corridors_found = unique.len();
    let mut candidates = unique.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.search_cost_seconds
            .cmp(&right.search_cost_seconds)
            .then(left.rides.cmp(&right.rides))
            .then_with(|| {
                corridor_order_key(&left.corridor).cmp(&corridor_order_key(&right.corridor))
            })
    });
    let mut chosen = HashSet::new();
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
                chosen.insert(index);
                best_with_fewer_rides = duration;
            }
        }
    }
    let mut mode_sets = chosen
        .iter()
        .map(|&index| corridor_modes(network, &candidates[index].corridor))
        .collect::<HashSet<_>>();
    for (index, candidate) in candidates.iter().enumerate() {
        if chosen.len() >= limit {
            break;
        }
        if mode_sets.insert(corridor_modes(network, &candidate.corridor)) {
            chosen.insert(index);
        }
    }
    let mut sequences = chosen
        .iter()
        .map(|&index| candidates[index].corridor.pattern_sequence())
        .collect::<HashSet<_>>();
    for (index, candidate) in candidates.iter().enumerate() {
        if chosen.len() >= limit {
            break;
        }
        if sequences.insert(candidate.corridor.pattern_sequence()) {
            chosen.insert(index);
        }
    }
    for index in 0..candidates.len() {
        if chosen.len() >= limit {
            break;
        }
        chosen.insert(index);
    }
    candidates
        .into_iter()
        .enumerate()
        .filter(|(index, _)| chosen.contains(index))
        .map(|(_, candidate)| candidate.corridor)
        .collect()
}

fn equivalent_legs_for(
    network: &TransitRuntime,
    topology: &TransitTopologyIndex,
    patterns: &CorridorPatternIndex,
    corridors: &[StaticCorridor],
) -> HashMap<EquivalentLegKey, Vec<StaticTransitLeg>> {
    let mut equivalent_legs = HashMap::<EquivalentLegKey, Vec<StaticTransitLeg>>::new();
    for leg in corridors.iter().flat_map(StaticCorridor::transit_legs) {
        let key = equivalent_leg_key(network, leg);
        if equivalent_legs.contains_key(&key) {
            continue;
        }
        let mut legs = Vec::new();
        for position in topology.occurrences_at(&leg.board_stop_id) {
            let Some(group_index) = patterns.group_index(&position.pattern_id) else {
                continue;
            };
            let group = patterns.group(group_index);
            if group.mode != key.mode {
                continue;
            }
            let Some(offset) = group.stops[position.stop_index + 1..]
                .iter()
                .position(|stop_id| *stop_id == leg.alight_stop_id)
            else {
                continue;
            };
            let alight_index = position.stop_index + 1 + offset;
            let mut first = None;
            let mut mixed = false;
            for segment in &group.segments[position.stop_index..alight_index] {
                mixed |= first.is_some_and(|known| known != segment.source);
                first.get_or_insert(segment.source);
            }
            legs.push(StaticTransitLeg {
                pattern_id: group.pattern_id.clone(),
                board_stop_id: leg.board_stop_id.clone(),
                board_index: position.stop_index,
                alight_stop_id: leg.alight_stop_id.clone(),
                alight_index,
                cost_source: leg_cost_source(first, mixed),
            });
        }
        if !legs.iter().any(|known| known == leg) {
            legs.push(leg.clone());
        }
        equivalent_legs.insert(key, legs);
    }
    equivalent_legs
}

fn corridor_modes(network: &TransitRuntime, corridor: &StaticCorridor) -> Vec<TransitMode> {
    let mut modes = corridor
        .transit_legs()
        .map(|leg| {
            network
                .route_of(network.required_pattern(&leg.pattern_id))
                .mode
        })
        .collect::<Vec<_>>();
    modes.sort_unstable();
    modes.dedup();
    modes
}

fn corridor_order_key(corridor: &StaticCorridor) -> (&str, &str, String) {
    (
        corridor.access_stop_id.as_str(),
        corridor.egress_stop_id.as_str(),
        format!("{:?}", corridor.steps),
    )
}

fn cost_source(steps: &[StaticCorridorStep]) -> StaticCostSource {
    let sources = steps
        .iter()
        .filter_map(|step| match step {
            StaticCorridorStep::Transit(leg) => Some(leg.cost_source),
            StaticCorridorStep::SurfaceTransfer { .. } => None,
        })
        .collect::<Vec<_>>();
    if sources.contains(&StaticCostSource::Mixed) {
        return StaticCostSource::Mixed;
    }
    match (
        sources.contains(&StaticCostSource::ExactSchedule),
        sources.contains(&StaticCostSource::DerivedBus),
        sources.contains(&StaticCostSource::OptimisticEstimate),
    ) {
        (true, false, false) => StaticCostSource::ExactSchedule,
        (false, true, false) => StaticCostSource::DerivedBus,
        (false, false, _) => StaticCostSource::OptimisticEstimate,
        _ => StaticCostSource::Mixed,
    }
}
