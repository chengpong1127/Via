use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use crate::routing::topology::PatternPosition;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::TransitMode,
    transit_runtime::TransitRuntime,
};

use super::{
    frontier::StaticState,
    heuristic::{EndpointModePair, sorted_mode_pairs},
    types::{
        CorridorSignature, EquivalentCorridorSignature, EquivalentLegKey,
        RetainedModeSequenceStats, RetainedPatternSequenceStats, StaticCorridor, StaticCostSource,
        StaticSearchStats, StaticTransitLeg, equivalent_leg_key, equivalent_steps,
    },
};

#[derive(Clone, Debug, Default)]
pub struct RetainedCorridorSet {
    retained: Vec<StaticCorridor>,
    equivalent_legs: HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    timing_positions: Vec<PatternPosition>,
}

impl RetainedCorridorSet {
    #[cfg(test)]
    pub(in crate::routing) fn from_retained_for_test(
        network: &TransitRuntime,
        retained: Vec<StaticCorridor>,
    ) -> Self {
        Self::from_retained(network, &HashMap::new(), retained).0
    }

    pub(super) fn from_retained(
        network: &TransitRuntime,
        equivalent_legs: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        retained: Vec<StaticCorridor>,
    ) -> (Self, usize, usize) {
        let retained_equivalent_legs = retained
            .iter()
            .flat_map(StaticCorridor::transit_legs)
            .filter_map(|leg| {
                let key = equivalent_leg_key(network, leg)?;
                equivalent_legs
                    .get(&key)
                    .map(|alternatives| (key, alternatives.clone()))
            })
            .collect::<HashMap<_, _>>();
        let mut timing_positions = HashSet::<PatternPosition>::new();
        let mut selected_pattern_ids = HashSet::new();
        let mut selected_bus_stop_ids = HashSet::new();
        for corridor in &retained {
            for leg in corridor.transit_legs() {
                for alternative in
                    Self::equivalent_legs_from(network, &retained_equivalent_legs, leg)
                {
                    timing_positions.insert(PatternPosition {
                        pattern_id: alternative.pattern_id.clone(),
                        stop_index: alternative.board_index,
                    });
                    selected_pattern_ids.insert(alternative.pattern_id.clone());
                    if network
                        .pattern(&alternative.pattern_id)
                        .is_some_and(|pattern| network.route_of(pattern).mode == TransitMode::Bus)
                    {
                        selected_bus_stop_ids.insert(alternative.board_stop_id.clone());
                    }
                }
            }
        }

        let mut timing_positions = timing_positions.into_iter().collect::<Vec<_>>();
        timing_positions.sort_by(|left, right| {
            left.pattern_id
                .as_str()
                .cmp(right.pattern_id.as_str())
                .then(left.stop_index.cmp(&right.stop_index))
        });

        (
            Self {
                retained,
                equivalent_legs: retained_equivalent_legs,
                timing_positions,
            },
            selected_pattern_ids.len(),
            selected_bus_stop_ids.len(),
        )
    }

    pub fn retained(&self) -> &[StaticCorridor] {
        &self.retained
    }

    pub fn equivalent_legs_for(
        &self,
        network: &TransitRuntime,
        leg: &StaticTransitLeg,
    ) -> Vec<StaticTransitLeg> {
        Self::equivalent_legs_from(network, &self.equivalent_legs, leg)
    }

    pub fn is_equivalent_leg(
        &self,
        network: &TransitRuntime,
        retained: &StaticTransitLeg,
        alternative: &StaticTransitLeg,
    ) -> bool {
        equivalent_leg_key(network, retained).is_some()
            && equivalent_leg_key(network, retained) == equivalent_leg_key(network, alternative)
    }

    fn equivalent_legs_from(
        network: &TransitRuntime,
        equivalent_legs: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        leg: &StaticTransitLeg,
    ) -> Vec<StaticTransitLeg> {
        let mut alternatives = equivalent_leg_key(network, leg)
            .and_then(|key| equivalent_legs.get(&key))
            .cloned()
            .unwrap_or_else(|| vec![leg.clone()]);
        alternatives.sort_by(|left, right| {
            left.pattern_id
                .as_str()
                .cmp(right.pattern_id.as_str())
                .then(left.board_index.cmp(&right.board_index))
                .then(left.alight_index.cmp(&right.alight_index))
        });
        alternatives
    }

    pub fn timing_positions(&self) -> impl Iterator<Item = PatternPosition> + '_ {
        self.timing_positions.iter().cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.retained.is_empty()
    }
}

fn retained_pattern_sequence_groups(
    network: &TransitRuntime,
    corridors: &[StaticCorridor],
) -> Vec<RetainedPatternSequenceStats> {
    let mut groups =
        HashMap::<(Vec<TransitPatternId>, Vec<TransitMode>), RetainedPatternSequenceStats>::new();
    for corridor in corridors {
        let pattern_sequence = corridor.pattern_sequence();
        let mode_sequence = pattern_sequence
            .iter()
            .filter_map(|pattern_id| {
                network
                    .pattern(pattern_id)
                    .map(|pattern| network.route_of(pattern).mode)
            })
            .collect::<Vec<_>>();
        let key = (pattern_sequence.clone(), mode_sequence.clone());
        groups
            .entry(key)
            .and_modify(|group| {
                group.corridors += 1;
                group.best_duration_seconds = group
                    .best_duration_seconds
                    .min(corridor.theoretical_duration_seconds);
                group.worst_duration_seconds = group
                    .worst_duration_seconds
                    .max(corridor.theoretical_duration_seconds);
            })
            .or_insert_with(|| RetainedPatternSequenceStats {
                pattern_sequence,
                mode_sequence,
                corridors: 1,
                best_duration_seconds: corridor.theoretical_duration_seconds,
                worst_duration_seconds: corridor.theoretical_duration_seconds,
            });
    }
    let mut groups = groups.into_values().collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .corridors
            .cmp(&left.corridors)
            .then(
                left.pattern_sequence
                    .len()
                    .cmp(&right.pattern_sequence.len()),
            )
            .then_with(|| {
                left.pattern_sequence
                    .iter()
                    .map(TransitPatternId::as_str)
                    .cmp(right.pattern_sequence.iter().map(TransitPatternId::as_str))
            })
    });
    groups
}

fn retained_mode_sequence_groups(
    pattern_groups: &[RetainedPatternSequenceStats],
) -> Vec<RetainedModeSequenceStats> {
    let mut groups = HashMap::<Vec<TransitMode>, RetainedModeSequenceStats>::new();
    for pattern_group in pattern_groups {
        groups
            .entry(pattern_group.mode_sequence.clone())
            .and_modify(|group| {
                group.corridors += pattern_group.corridors;
                group.best_duration_seconds = group
                    .best_duration_seconds
                    .min(pattern_group.best_duration_seconds);
                group.worst_duration_seconds = group
                    .worst_duration_seconds
                    .max(pattern_group.worst_duration_seconds);
            })
            .or_insert_with(|| RetainedModeSequenceStats {
                mode_sequence: pattern_group.mode_sequence.clone(),
                corridors: pattern_group.corridors,
                best_duration_seconds: pattern_group.best_duration_seconds,
                worst_duration_seconds: pattern_group.worst_duration_seconds,
            });
    }
    let mut groups = groups.into_values().collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .corridors
            .cmp(&left.corridors)
            .then(left.mode_sequence.len().cmp(&right.mode_sequence.len()))
            .then(left.mode_sequence.cmp(&right.mode_sequence))
    });
    groups
}

pub(super) struct CorridorRetention {
    retained: HashMap<CorridorSignature, StaticCorridor>,
    best_by_mode_transition_sequence: HashMap<Vec<TransitMode>, StaticCorridor>,
    all_unique: HashSet<CorridorSignature>,
    all_structurally_unique: HashSet<EquivalentCorridorSignature>,
    complete_mode_pairs: HashSet<EndpointModePair>,
    limit: usize,
}

impl CorridorRetention {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            retained: HashMap::new(),
            best_by_mode_transition_sequence: HashMap::new(),
            all_unique: HashSet::new(),
            all_structurally_unique: HashSet::new(),
            complete_mode_pairs: HashSet::new(),
            limit,
        }
    }

    pub(super) fn record_complete(
        &mut self,
        state: &StaticState,
        egress_by_stop: &HashMap<BoardingPointId, Vec<u32>>,
        max_journey_duration_seconds: u32,
        stats: &mut StaticSearchStats,
        network: &TransitRuntime,
    ) {
        for egress_seconds in egress_by_stop.get(&state.stop_id).into_iter().flatten() {
            let duration = state.elapsed_seconds.saturating_add(*egress_seconds);
            if duration > max_journey_duration_seconds {
                continue;
            }
            stats.static_complete_candidates_found += 1;
            let corridor = StaticCorridor {
                access_stop_id: state.access_stop_id.clone(),
                egress_stop_id: state.stop_id.clone(),
                theoretical_duration_seconds: duration,
                cost_source: state.cost_source(),
                steps: state.steps.clone(),
            };
            let signature = corridor.signature();
            if self.all_unique.insert(signature.clone()) {
                stats.static_unique_corridors_found += 1;
            }
            if let Some(steps) = equivalent_steps(network, &corridor.steps) {
                let structural_signature = EquivalentCorridorSignature {
                    access_stop_id: corridor.access_stop_id.clone(),
                    steps,
                    egress_stop_id: corridor.egress_stop_id.clone(),
                };
                if self.all_structurally_unique.insert(structural_signature) {
                    stats.static_unique_structural_corridors_found += 1;
                }
            }
            let modes = corridor
                .transit_legs()
                .filter_map(|leg| {
                    network
                        .pattern(&leg.pattern_id)
                        .map(|pattern| network.route_of(pattern).mode)
                })
                .collect::<Vec<_>>();
            if let (Some(first), Some(last)) = (modes.first(), modes.last()) {
                self.complete_mode_pairs.insert(EndpointModePair {
                    first: *first,
                    last: *last,
                });
            }
            let mut transition_sequence = Vec::new();
            for mode in modes {
                if transition_sequence.last() != Some(&mode) {
                    transition_sequence.push(mode);
                }
            }
            let family_replace = self
                .best_by_mode_transition_sequence
                .get(&transition_sequence)
                .is_none_or(|known| corridor_order(&corridor, known).is_lt());
            if family_replace {
                self.best_by_mode_transition_sequence
                    .insert(transition_sequence, corridor.clone());
            }
            let replace = self.retained.get(&signature).is_none_or(|known| {
                corridor.theoretical_duration_seconds < known.theoretical_duration_seconds
            });
            if replace {
                self.retained.insert(signature, corridor);
                self.trim();
            }
        }
    }

    pub(super) fn kth_duration(&self) -> Option<u32> {
        if self.retained.len() < self.limit {
            return None;
        }
        self.retained
            .values()
            .map(|corridor| corridor.theoretical_duration_seconds)
            .max()
    }

    fn trim(&mut self) {
        if self.retained.len() <= self.limit {
            return;
        }
        let worst = self
            .retained
            .iter()
            .max_by(|(_, left), (_, right)| corridor_order(left, right))
            .map(|(signature, _)| signature.clone());
        if let Some(worst) = worst {
            self.retained.remove(&worst);
        }
    }

    pub(super) fn finish(
        self,
        network: &TransitRuntime,
        equivalent_legs: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        stats: &mut StaticSearchStats,
    ) -> RetainedCorridorSet {
        stats.static_complete_endpoint_mode_pairs = sorted_mode_pairs(&self.complete_mode_pairs);
        let mut global_candidates = self.retained.into_values().collect::<Vec<_>>();
        global_candidates.sort_by(corridor_order);
        let mut structural_families = self
            .best_by_mode_transition_sequence
            .into_values()
            .collect::<Vec<_>>();
        structural_families.sort_by(corridor_order);
        let mut selected_structures = HashSet::new();
        let mut selected = Vec::new();
        for corridor in structural_families.into_iter().chain(global_candidates) {
            if selected.len() >= self.limit {
                break;
            }
            let Some(steps) = equivalent_steps(network, &corridor.steps) else {
                continue;
            };
            let structure = EquivalentCorridorSignature {
                access_stop_id: corridor.access_stop_id.clone(),
                steps,
                egress_stop_id: corridor.egress_stop_id.clone(),
            };
            if selected_structures.insert(structure) {
                selected.push(corridor);
            }
        }
        selected.sort_by(corridor_order);
        let (surface, pattern_count, bus_stop_count) =
            RetainedCorridorSet::from_retained(network, equivalent_legs, selected);
        stats.static_retained_pattern_sequence_groups =
            retained_pattern_sequence_groups(network, surface.retained());
        stats.static_retained_mode_sequence_groups =
            retained_mode_sequence_groups(&stats.static_retained_pattern_sequence_groups);
        stats.static_corridors_retained = surface.retained().len();
        stats.static_corridor_patterns = pattern_count;
        stats.static_corridor_positions = surface.timing_positions().count();
        stats.static_corridor_bus_stops = bus_stop_count;
        stats.kth_static_duration_seconds = surface
            .retained()
            .get(self.limit.saturating_sub(1))
            .map(|corridor| corridor.theoretical_duration_seconds);
        for corridor in surface.retained() {
            match corridor.cost_source {
                StaticCostSource::ExactSchedule => stats.exact_schedule_cost += 1,
                StaticCostSource::DerivedBus => stats.derived_bus_cost += 1,
                StaticCostSource::OptimisticEstimate => stats.estimated_cost += 1,
                StaticCostSource::Mixed => stats.mixed_cost += 1,
            }
        }
        surface
    }
}

pub(super) fn corridor_order(left: &StaticCorridor, right: &StaticCorridor) -> Ordering {
    left.theoretical_duration_seconds
        .cmp(&right.theoretical_duration_seconds)
        .then_with(|| format!("{:?}", left.signature()).cmp(&format!("{:?}", right.signature())))
}
