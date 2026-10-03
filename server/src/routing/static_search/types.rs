use std::collections::BTreeMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::TransitMode,
    transit_runtime::TransitRuntime,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticCorridor {
    pub access_stop_id: BoardingPointId,
    pub egress_stop_id: BoardingPointId,
    pub theoretical_duration_seconds: u32,
    pub cost_source: StaticCostSource,
    pub steps: Vec<StaticCorridorStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::routing) struct CorridorAnchors {
    stops: Vec<BoardingPointId>,
}

impl CorridorAnchors {
    pub(in crate::routing) fn stops(&self) -> &[BoardingPointId] {
        &self.stops
    }
}

impl StaticCorridor {
    pub(in crate::routing) fn anchors(&self) -> Option<CorridorAnchors> {
        let mut stops = vec![self.access_stop_id.clone()];
        for step in &self.steps {
            let (from, to) = match step {
                StaticCorridorStep::Transit(leg) => (&leg.board_stop_id, &leg.alight_stop_id),
                StaticCorridorStep::SurfaceTransfer { from, to, .. } => (from, to),
            };
            if stops.last() != Some(from) {
                return None;
            }
            stops.push(to.clone());
        }
        (stops.last() == Some(&self.egress_stop_id)).then_some(CorridorAnchors { stops })
    }

    pub fn transit_legs(&self) -> impl Iterator<Item = &StaticTransitLeg> {
        self.steps.iter().filter_map(|step| match step {
            StaticCorridorStep::Transit(leg) => Some(leg),
            StaticCorridorStep::SurfaceTransfer { .. } => None,
        })
    }

    pub fn pattern_sequence(&self) -> Vec<TransitPatternId> {
        self.transit_legs()
            .map(|leg| leg.pattern_id.clone())
            .collect()
    }

    pub(super) fn signature(&self) -> CorridorSignature {
        CorridorSignature {
            access_stop_id: self.access_stop_id.clone(),
            steps: structural_steps(&self.steps),
            egress_stop_id: self.egress_stop_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct CorridorSignature {
    pub(super) access_stop_id: BoardingPointId,
    pub(super) steps: Vec<StructuralStep>,
    pub(super) egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PartialSignature {
    pub(super) access_stop_id: BoardingPointId,
    pub(super) steps: Vec<StructuralStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum StructuralStep {
    Transit {
        pattern_id: TransitPatternId,
        board_stop_id: BoardingPointId,
        board_index: usize,
        alight_stop_id: BoardingPointId,
        alight_index: usize,
    },
    SurfaceTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct EquivalentCorridorSignature {
    pub(super) access_stop_id: BoardingPointId,
    pub(super) steps: Vec<EquivalentStep>,
    pub(super) egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ContinuationSignature {
    pub(super) access_stop_id: BoardingPointId,
    pub(super) mode_transition_sequence: Vec<TransitMode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct EquivalentLegKey {
    pub(super) mode: TransitMode,
    pub(super) board_stop_id: BoardingPointId,
    pub(super) alight_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum EquivalentStep {
    Transit {
        mode: TransitMode,
        board_stop_id: BoardingPointId,
        alight_stop_id: BoardingPointId,
    },
    SurfaceTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

pub(super) fn equivalent_steps(
    network: &TransitRuntime,
    steps: &[StaticCorridorStep],
) -> Option<Vec<EquivalentStep>> {
    steps
        .iter()
        .map(|step| match step {
            StaticCorridorStep::Transit(leg) => Some(EquivalentStep::Transit {
                mode: network.route_of(network.pattern(&leg.pattern_id)?).mode,
                board_stop_id: leg.board_stop_id.clone(),
                alight_stop_id: leg.alight_stop_id.clone(),
            }),
            StaticCorridorStep::SurfaceTransfer { from, to, .. } => {
                Some(EquivalentStep::SurfaceTransfer {
                    from: from.clone(),
                    to: to.clone(),
                })
            }
        })
        .collect()
}

pub(super) fn equivalent_leg_key(
    network: &TransitRuntime,
    leg: &StaticTransitLeg,
) -> Option<EquivalentLegKey> {
    let pattern = network.pattern(&leg.pattern_id)?;
    Some(EquivalentLegKey {
        mode: network.route_of(pattern).mode,
        board_stop_id: leg.board_stop_id.clone(),
        alight_stop_id: leg.alight_stop_id.clone(),
    })
}

pub(super) fn structural_steps(steps: &[StaticCorridorStep]) -> Vec<StructuralStep> {
    steps
        .iter()
        .map(|step| match step {
            StaticCorridorStep::Transit(leg) => StructuralStep::Transit {
                pattern_id: leg.pattern_id.clone(),
                board_stop_id: leg.board_stop_id.clone(),
                board_index: leg.board_index,
                alight_stop_id: leg.alight_stop_id.clone(),
                alight_index: leg.alight_index,
            },
            StaticCorridorStep::SurfaceTransfer { from, to, .. } => {
                StructuralStep::SurfaceTransfer {
                    from: from.clone(),
                    to: to.clone(),
                }
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StaticSearchStats {
    pub static_search_states_expanded: usize,
    pub static_unattributed_states_expanded: usize,
    pub static_pattern_scans: usize,
    pub static_positions_scanned: usize,
    pub static_walking_transfer_expansions: usize,
    pub static_duplicate_states_rejected: usize,
    pub static_dominance_states_rejected: usize,
    pub static_structurally_equivalent_states_rejected: usize,
    pub static_required_endpoint_mode_pairs: Vec<(TransitMode, TransitMode)>,
    pub static_complete_endpoint_mode_pairs: Vec<(TransitMode, TransitMode)>,
    pub static_complete_candidates_found: usize,
    pub static_unique_corridors_found: usize,
    pub static_unique_structural_corridors_found: usize,
    pub static_corridors_retained: usize,
    pub static_retained_pattern_sequence_groups: Vec<RetainedPatternSequenceStats>,
    pub static_retained_mode_sequence_groups: Vec<RetainedModeSequenceStats>,
    pub static_mode_stats: BTreeMap<TransitMode, TransitModeSearchStats>,
    pub static_expanded_stop_occurrence_samples: usize,
    pub static_expanded_stop_occurrences_total: usize,
    pub static_expanded_stop_pattern_occurrences_total: usize,
    pub static_expanded_stop_occurrences_max: usize,
    pub static_expanded_stop_occurrences_average_milli: usize,
    pub static_expanded_stop_pattern_occurrences_average_milli: usize,
    pub static_expanded_stop_pattern_occurrences_max: usize,
    pub static_expanded_egress_states: usize,
    pub static_expanded_terminal_transfer_source_states: usize,
    pub static_expanded_states_by_leg_count: Vec<(usize, usize)>,
    pub static_most_expanded_stops: Vec<(BoardingPointId, usize)>,
    pub static_corridor_patterns: usize,
    pub static_corridor_positions: usize,
    pub static_corridor_bus_stops: usize,
    pub static_search_latency_ms: u64,
    pub static_pattern_index_build_latency_us: u64,
    pub static_pattern_index_pattern_edges_scanned: usize,
    pub static_pattern_index_positions_scanned: usize,
    pub static_ride_profile_build_latency_us: u64,
    pub static_ride_profile_patterns_scanned: usize,
    pub static_ride_profile_positions_scanned: usize,
    pub static_lower_bound_latency_us: u64,
    pub static_lower_bound_nodes_settled: usize,
    pub static_lower_bound_pattern_edges_scanned: usize,
    pub static_lower_bound_transfer_edges_scanned: usize,
    pub static_forward_search_latency_us: u64,
    pub static_retention_latency_us: u64,
    pub kth_static_duration_seconds: Option<u32>,
    pub exact_schedule_cost: usize,
    pub derived_bus_cost: usize,
    pub estimated_cost: usize,
    pub mixed_cost: usize,
    pub static_search_limit_reached: bool,
    pub static_top_k_early_terminated: bool,
    pub deadline_exceeded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitModeSearchStats {
    pub states_expanded: usize,
    pub pattern_scans: usize,
    pub downstream_positions_scanned: usize,
    pub distinct_pattern_ids: usize,
    pub distinct_stop_sequences: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedPatternSequenceStats {
    pub pattern_sequence: Vec<TransitPatternId>,
    pub mode_sequence: Vec<TransitMode>,
    pub corridors: usize,
    pub best_duration_seconds: u32,
    pub worst_duration_seconds: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedModeSequenceStats {
    pub mode_sequence: Vec<TransitMode>,
    pub corridors: usize,
    pub best_duration_seconds: u32,
    pub worst_duration_seconds: u32,
}
