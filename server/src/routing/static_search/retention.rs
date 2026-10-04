use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use crate::routing::topology::PatternPosition;

use crate::domain::{ids::BoardingPointId, transit::TransitMode, transit_runtime::TransitRuntime};

use super::{
    frontier::StaticState,
    types::{
        CorridorSignature, EquivalentCorridorSignature, EquivalentLegKey, StaticCorridor,
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
                let key = equivalent_leg_key(network, leg);
                equivalent_legs
                    .get(&key)
                    .filter(|legs| legs.len() > 1)
                    .map(|equivalent_legs| (key, equivalent_legs.clone()))
            })
            .collect::<HashMap<_, _>>();
        let mut timing_positions = HashSet::<PatternPosition>::new();
        let mut selected_pattern_ids = HashSet::new();
        let mut selected_bus_stop_ids = HashSet::new();
        for corridor in &retained {
            for leg in corridor.transit_legs() {
                for equivalent_leg in
                    Self::equivalent_legs_from(network, &retained_equivalent_legs, leg)
                {
                    timing_positions.insert(PatternPosition {
                        pattern_id: equivalent_leg.pattern_id.clone(),
                        stop_index: equivalent_leg.board_index,
                    });
                    selected_pattern_ids.insert(equivalent_leg.pattern_id.clone());
                    if network
                        .route_of(network.required_pattern(&equivalent_leg.pattern_id))
                        .mode
                        == TransitMode::Bus
                    {
                        selected_bus_stop_ids.insert(equivalent_leg.board_stop_id.clone());
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
        equivalent_leg: &StaticTransitLeg,
    ) -> bool {
        equivalent_leg_key(network, retained) == equivalent_leg_key(network, equivalent_leg)
    }

    fn equivalent_legs_from(
        network: &TransitRuntime,
        equivalent_legs: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        leg: &StaticTransitLeg,
    ) -> Vec<StaticTransitLeg> {
        let mut equivalent_legs = equivalent_legs
            .get(&equivalent_leg_key(network, leg))
            .cloned()
            .unwrap_or_else(|| vec![leg.clone()]);
        equivalent_legs.sort_by(|left, right| {
            left.pattern_id
                .as_str()
                .cmp(right.pattern_id.as_str())
                .then(left.board_index.cmp(&right.board_index))
                .then(left.alight_index.cmp(&right.alight_index))
        });
        equivalent_legs
    }

    pub fn timing_positions(&self) -> impl Iterator<Item = PatternPosition> + '_ {
        self.timing_positions.iter().cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.retained.is_empty()
    }
}

pub(super) struct CorridorRetention {
    retained: HashMap<CorridorSignature, StaticCorridor>,
    best_by_mode_transition_sequence: HashMap<Vec<TransitMode>, StaticCorridor>,
    all_unique: HashSet<CorridorSignature>,
    limit: usize,
}

impl CorridorRetention {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            retained: HashMap::new(),
            best_by_mode_transition_sequence: HashMap::new(),
            all_unique: HashSet::new(),
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

            let corridor = StaticCorridor {
                access_stop_id: state.access_stop_id.clone(),
                egress_stop_id: state.stop_id.clone(),
                theoretical_duration_seconds: duration,
                cost_source: state.cost_source(),
                steps: state.steps.clone(),
            };
            let signature = corridor.signature();
            if self.all_unique.insert(signature.clone()) {
                stats.unique_corridors_found += 1;
            }
            let modes = corridor
                .transit_legs()
                .map(|leg| {
                    network
                        .route_of(network.required_pattern(&leg.pattern_id))
                        .mode
                })
                .collect::<Vec<_>>();
            let mut transition_sequence = Vec::new();
            for mode in modes {
                if transition_sequence.last() != Some(&mode) {
                    transition_sequence.push(mode);
                }
            }
            let replace_mode_sequence_corridor = self
                .best_by_mode_transition_sequence
                .get(&transition_sequence)
                .is_none_or(|known| corridor_order(&corridor, known).is_lt());
            if replace_mode_sequence_corridor {
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
        let mut global_corridors = self.retained.into_values().collect::<Vec<_>>();
        global_corridors.sort_by(corridor_order);
        let mut best_mode_sequence_corridors = self
            .best_by_mode_transition_sequence
            .into_values()
            .collect::<Vec<_>>();
        best_mode_sequence_corridors.sort_by(corridor_order);
        let mut selected_structures = HashSet::new();
        let mut selected = Vec::new();
        for corridor in best_mode_sequence_corridors
            .into_iter()
            .chain(global_corridors)
        {
            if selected.len() >= self.limit {
                break;
            }
            let steps = equivalent_steps(network, &corridor.steps);
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
        let (retained_corridors, pattern_count, bus_stop_count) =
            RetainedCorridorSet::from_retained(network, equivalent_legs, selected);
        stats.corridors_retained = retained_corridors.retained().len();
        stats.corridor_patterns = pattern_count;
        stats.corridor_positions = retained_corridors.timing_positions().count();
        stats.corridor_bus_stops = bus_stop_count;
        stats.kth_duration_seconds = retained_corridors
            .retained()
            .get(self.limit.saturating_sub(1))
            .map(|corridor| corridor.theoretical_duration_seconds);
        retained_corridors
    }
}

pub(super) fn corridor_order(left: &StaticCorridor, right: &StaticCorridor) -> Ordering {
    left.theoretical_duration_seconds
        .cmp(&right.theoretical_duration_seconds)
        .then_with(|| format!("{:?}", left.signature()).cmp(&format!("{:?}", right.signature())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ids::TransitPatternId,
        transit::{TransitNetwork, TransitPattern},
    };
    use crate::routing::static_search::types::{StaticCorridorStep, StaticCostSource};

    #[test]
    fn finalized_equivalent_leg_storage_preserves_lookup_and_timing() {
        for pattern_ids in [&["bus-a"][..], &["bus-b", "bus-a"][..]] {
            let board_stop_id = BoardingPointId::new("origin");
            let alight_stop_id = BoardingPointId::new("destination");
            let network = TransitRuntime::new(std::sync::Arc::new(TransitNetwork::fixture(
                time::Time::MIDNIGHT,
                Vec::new(),
                pattern_ids
                    .iter()
                    .map(|pattern_id| {
                        TransitPattern::fixture(
                            TransitPatternId::new(*pattern_id),
                            TransitMode::Bus,
                            *pattern_id,
                            "destination",
                            vec![board_stop_id.clone(), alight_stop_id.clone()],
                        )
                    })
                    .collect(),
                Vec::new(),
            )));
            let legs = pattern_ids
                .iter()
                .map(|pattern_id| StaticTransitLeg {
                    pattern_id: TransitPatternId::new(*pattern_id),
                    board_stop_id: board_stop_id.clone(),
                    board_index: 0,
                    alight_stop_id: alight_stop_id.clone(),
                    alight_index: 1,
                    cost_source: StaticCostSource::OptimisticEstimate,
                })
                .collect::<Vec<_>>();
            let retained_leg = &legs[0];
            let key = equivalent_leg_key(&network, retained_leg);
            let equivalent_legs = HashMap::from([(key.clone(), legs.clone())]);
            let corridor = StaticCorridor {
                access_stop_id: board_stop_id,
                egress_stop_id: alight_stop_id,
                theoretical_duration_seconds: 100,
                cost_source: StaticCostSource::OptimisticEstimate,
                steps: vec![StaticCorridorStep::Transit(retained_leg.clone())],
            };

            let (finalized, pattern_count, bus_stop_count) = RetainedCorridorSet::from_retained(
                &network,
                &equivalent_legs,
                vec![corridor.clone()],
            );

            assert_eq!(finalized.retained(), &[corridor]);
            assert_eq!(equivalent_legs[&key], legs);
            if legs.len() == 1 {
                assert!(finalized.equivalent_legs.is_empty());
                assert_eq!(
                    finalized.equivalent_legs_for(&network, retained_leg),
                    vec![retained_leg.clone()]
                );
            } else {
                assert_eq!(finalized.equivalent_legs, equivalent_legs);
            }
            let expected_legs =
                RetainedCorridorSet::equivalent_legs_from(&network, &equivalent_legs, retained_leg);
            assert_eq!(
                finalized.equivalent_legs_for(&network, retained_leg),
                expected_legs
            );
            assert_eq!(
                finalized.timing_positions().collect::<Vec<_>>(),
                expected_legs
                    .iter()
                    .map(|leg| PatternPosition {
                        pattern_id: leg.pattern_id.clone(),
                        stop_index: leg.board_index,
                    })
                    .collect::<Vec<_>>()
            );
            assert_eq!(pattern_count, legs.len());
            assert_eq!(bus_stop_count, 1);
        }
    }
}
