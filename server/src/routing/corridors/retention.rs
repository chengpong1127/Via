use std::collections::{HashMap, HashSet};

use crate::domain::{
    transit::network::TransitMode,
    transit::runtime::{PatternPosition, TransitRuntime},
};

use super::{
    corridor::Corridor,
    types::{EquivalentLegKey, StaticTransitLeg, equivalent_leg_key},
};

#[derive(Clone, Debug, Default)]
pub struct RetainedCorridorSet {
    retained_corridors: Vec<Corridor>,
    equivalent_legs_by_key: HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    timing_positions: Vec<PatternPosition>,
}

impl RetainedCorridorSet {
    #[cfg(test)]
    pub(in crate::routing) fn from_retained_for_test(
        transit: &TransitRuntime,
        retained_corridors: Vec<Corridor>,
    ) -> Self {
        Self::from_retained(transit, &HashMap::new(), retained_corridors).0
    }

    pub(super) fn from_retained(
        transit: &TransitRuntime,
        equivalent_legs_by_key: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        retained_corridors: Vec<Corridor>,
    ) -> (Self, usize, usize) {
        let retained_equivalent_legs_by_key = retained_corridors
            .iter()
            .flat_map(Corridor::transit_legs)
            .filter_map(|leg| {
                let key = equivalent_leg_key(transit, leg);
                equivalent_legs_by_key
                    .get(&key)
                    .filter(|legs| legs.len() > 1)
                    .map(|equivalent_legs| (key, equivalent_legs.clone()))
            })
            .collect::<HashMap<_, _>>();
        let mut timing_positions = HashSet::<PatternPosition>::new();
        let mut selected_pattern_ids = HashSet::new();
        let mut selected_bus_stop_ids = HashSet::new();
        for corridor in &retained_corridors {
            for leg in corridor.transit_legs() {
                for equivalent_leg in
                    Self::equivalent_legs_from(transit, &retained_equivalent_legs_by_key, leg)
                {
                    timing_positions.insert(PatternPosition {
                        pattern_id: equivalent_leg.pattern_id.clone(),
                        stop_index: equivalent_leg.board_index,
                    });
                    selected_pattern_ids.insert(equivalent_leg.pattern_id.clone());
                    if transit
                        .required_route(
                            &transit
                                .required_pattern(&equivalent_leg.pattern_id)
                                .route_id,
                        )
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
                retained_corridors,
                equivalent_legs_by_key: retained_equivalent_legs_by_key,
                timing_positions,
            },
            selected_pattern_ids.len(),
            selected_bus_stop_ids.len(),
        )
    }

    pub fn retained_corridors(&self) -> &[Corridor] {
        &self.retained_corridors
    }

    pub fn equivalent_legs_for(
        &self,
        transit: &TransitRuntime,
        leg: &StaticTransitLeg,
    ) -> Vec<StaticTransitLeg> {
        Self::equivalent_legs_from(transit, &self.equivalent_legs_by_key, leg)
    }

    pub fn is_equivalent_leg(
        &self,
        transit: &TransitRuntime,
        retained_leg: &StaticTransitLeg,
        equivalent_leg: &StaticTransitLeg,
    ) -> bool {
        equivalent_leg_key(transit, retained_leg) == equivalent_leg_key(transit, equivalent_leg)
    }

    fn equivalent_legs_from(
        transit: &TransitRuntime,
        equivalent_legs_by_key: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        leg: &StaticTransitLeg,
    ) -> Vec<StaticTransitLeg> {
        let mut equivalent_legs = equivalent_legs_by_key
            .get(&equivalent_leg_key(transit, leg))
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
        self.retained_corridors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::BoardingPointId;
    use crate::domain::{
        ids::TransitPatternId,
        transit::network::{TransitNetwork, TransitPattern},
    };
    use crate::routing::corridors::types::StaticCostSource;

    #[test]
    fn finalized_equivalent_leg_storage_preserves_lookup_and_timing() {
        for pattern_ids in [&["bus-a"][..], &["bus-b", "bus-a"][..]] {
            let board_stop_id = BoardingPointId::new("origin");
            let alight_stop_id = BoardingPointId::new("destination");
            let transit = TransitRuntime::new(std::sync::Arc::new(TransitNetwork::fixture(
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
            let key = equivalent_leg_key(&transit, retained_leg);
            let equivalent_legs_by_key = HashMap::from([(key.clone(), legs.clone())]);
            let corridor = Corridor::from_transit_leg_for_test(retained_leg.clone());

            let (finalized, pattern_count, bus_stop_count) = RetainedCorridorSet::from_retained(
                &transit,
                &equivalent_legs_by_key,
                vec![corridor.clone()],
            );

            assert_eq!(finalized.retained_corridors(), &[corridor]);
            assert_eq!(equivalent_legs_by_key[&key], legs);
            if legs.len() == 1 {
                assert!(finalized.equivalent_legs_by_key.is_empty());
                assert_eq!(
                    finalized.equivalent_legs_for(&transit, retained_leg),
                    vec![retained_leg.clone()]
                );
            } else {
                assert_eq!(finalized.equivalent_legs_by_key, equivalent_legs_by_key);
            }
            let expected_legs = RetainedCorridorSet::equivalent_legs_from(
                &transit,
                &equivalent_legs_by_key,
                retained_leg,
            );
            assert_eq!(
                finalized.equivalent_legs_for(&transit, retained_leg),
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
