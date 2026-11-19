use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::network::TransitMode,
    transit::runtime::TransitRuntime,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticCostSource {
    ExactSchedule,
    DerivedBus,
    OptimisticEstimate,
    Mixed,
}

impl StaticCostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactSchedule => "exact_static_schedule",
            Self::DerivedBus => "derived_bus_static_cost",
            Self::OptimisticEstimate => "optimistic_estimate",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticTransitLeg {
    pub pattern_id: TransitPatternId,
    pub board_stop_id: BoardingPointId,
    pub board_index: usize,
    pub alight_stop_id: BoardingPointId,
    pub alight_index: usize,
    pub cost_source: StaticCostSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StaticCorridorStep {
    Transit(StaticTransitLeg),
    SurfaceTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
        walking_duration_seconds: u32,
        walking_distance_metres: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct EquivalentCorridorSignature {
    pub(super) access_stop_id: BoardingPointId,
    pub(super) steps: Vec<EquivalentStep>,
    pub(super) egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct EquivalentLegKey {
    pub(super) mode: TransitMode,
    pub(super) board_stop_id: BoardingPointId,
    pub(super) alight_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum EquivalentStep {
    Transit(EquivalentLegKey),
    SurfaceTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

pub(super) fn equivalent_steps(
    transit: &TransitRuntime,
    steps: &[StaticCorridorStep],
) -> Vec<EquivalentStep> {
    steps
        .iter()
        .map(|step| match step {
            StaticCorridorStep::Transit(leg) => {
                EquivalentStep::Transit(equivalent_leg_key(transit, leg))
            }
            StaticCorridorStep::SurfaceTransfer { from, to, .. } => {
                EquivalentStep::SurfaceTransfer {
                    from: from.clone(),
                    to: to.clone(),
                }
            }
        })
        .collect()
}

pub(super) fn equivalent_leg_key(
    transit: &TransitRuntime,
    leg: &StaticTransitLeg,
) -> EquivalentLegKey {
    let pattern = transit.required_pattern(&leg.pattern_id);
    EquivalentLegKey {
        mode: transit.required_route(&pattern.route_id).mode,
        board_stop_id: leg.board_stop_id.clone(),
        alight_stop_id: leg.alight_stop_id.clone(),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CorridorSearchStats {
    pub states_expanded: usize,
    pub unique_corridors_found: usize,
    pub corridors_retained: usize,
    pub search_latency_ms: u64,
}
