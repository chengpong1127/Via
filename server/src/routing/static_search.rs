//! Bounded static Top-K corridor generation.
//!
//! This phase scans canonical Patterns and their persisted segment profiles.
//! Preprocessing supplies exact, derived, or optimistic costs. These values
//! rank zero-wait candidate corridors; the request-local timing snapshot
//! remains authoritative for feasibility.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, HashMap, HashSet},
    time::Instant,
};

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::{SegmentTimeSource, SegmentTravelTime, TransitMode, TransitNetwork},
};

use super::{
    model::TransitQuery,
    topology::{PatternPosition, TransitTopologyIndex},
    transfers::TransferShortcutIndex,
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
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
        duration_seconds: u32,
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

impl StaticCorridor {
    pub fn transit_legs(&self) -> impl Iterator<Item = &StaticTransitLeg> {
        self.steps.iter().filter_map(|step| match step {
            StaticCorridorStep::Transit(leg) => Some(leg),
            StaticCorridorStep::WalkingTransfer { .. } => None,
        })
    }

    pub fn service_sequence(&self) -> Vec<TransitPatternId> {
        self.transit_legs()
            .map(|leg| leg.pattern_id.clone())
            .collect()
    }

    fn signature(&self) -> CorridorSignature {
        CorridorSignature {
            access_stop_id: self.access_stop_id.clone(),
            steps: structural_steps(&self.steps),
            egress_stop_id: self.egress_stop_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CorridorSignature {
    access_stop_id: BoardingPointId,
    steps: Vec<StructuralStep>,
    egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PartialSignature {
    access_stop_id: BoardingPointId,
    steps: Vec<StructuralStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum StructuralStep {
    Transit {
        pattern_id: TransitPatternId,
        board_stop_id: BoardingPointId,
        board_index: usize,
        alight_stop_id: BoardingPointId,
        alight_index: usize,
    },
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EquivalentCorridorSignature {
    access_stop_id: BoardingPointId,
    steps: Vec<EquivalentStep>,
    egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ContinuationSignature {
    access_stop_id: BoardingPointId,
    mode_transition_sequence: Vec<TransitMode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EquivalentLegKey {
    mode: TransitMode,
    board_stop_id: BoardingPointId,
    alight_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum EquivalentStep {
    Transit {
        mode: TransitMode,
        board_stop_id: BoardingPointId,
        alight_stop_id: BoardingPointId,
    },
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

fn equivalent_steps(
    network: &TransitNetwork,
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
            StaticCorridorStep::WalkingTransfer { from, to, .. } => {
                Some(EquivalentStep::WalkingTransfer {
                    from: from.clone(),
                    to: to.clone(),
                })
            }
        })
        .collect()
}

fn equivalent_leg_key(
    network: &TransitNetwork,
    leg: &StaticTransitLeg,
) -> Option<EquivalentLegKey> {
    let service = network.pattern(&leg.pattern_id)?;
    Some(EquivalentLegKey {
        mode: network.route_of(service).mode,
        board_stop_id: leg.board_stop_id.clone(),
        alight_stop_id: leg.alight_stop_id.clone(),
    })
}

fn structural_steps(steps: &[StaticCorridorStep]) -> Vec<StructuralStep> {
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
            StaticCorridorStep::WalkingTransfer { from, to, .. } => {
                StructuralStep::WalkingTransfer {
                    from: from.clone(),
                    to: to.clone(),
                }
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateBoarding {
    pub pattern_id: TransitPatternId,
    pub board_index: usize,
    pub min_remaining_legs: usize,
}

/// The small union surface consumed by snapshot loading and temporal search.
#[derive(Clone, Debug, Default)]
pub struct CandidateCorridor {
    retained: Vec<StaticCorridor>,
    boardings_by_stop: HashMap<BoardingPointId, Vec<CandidateBoarding>>,
    minimums_by_position: HashMap<PatternPosition, usize>,
    selected_service_ids: HashSet<TransitPatternId>,
    selected_bus_stop_ids: HashSet<BoardingPointId>,
    terminal_transfers: HashSet<(BoardingPointId, BoardingPointId)>,
}

impl CandidateCorridor {
    fn from_retained(
        network: &TransitNetwork,
        equivalent_legs: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        retained: Vec<StaticCorridor>,
    ) -> Self {
        let mut minimums_by_position = HashMap::<PatternPosition, usize>::new();
        let mut selected_service_ids = HashSet::new();
        let mut selected_bus_stop_ids = HashSet::new();
        let mut terminal_transfers = HashSet::new();
        for corridor in &retained {
            if let Some(StaticCorridorStep::WalkingTransfer { from, to, .. }) =
                corridor.steps.last()
            {
                terminal_transfers.insert((from.clone(), to.clone()));
            }
            let legs = corridor.transit_legs().collect::<Vec<_>>();
            for (index, leg) in legs.iter().enumerate() {
                let remaining = legs.len() - index;
                let alternatives = equivalent_leg_key(network, leg)
                    .and_then(|key| equivalent_legs.get(&key))
                    .cloned()
                    .unwrap_or_else(|| vec![(*leg).clone()]);
                for alternative in alternatives {
                    let position = PatternPosition {
                        pattern_id: alternative.pattern_id.clone(),
                        stop_index: alternative.board_index,
                    };
                    minimums_by_position
                        .entry(position)
                        .and_modify(|known| *known = (*known).min(remaining))
                        .or_insert(remaining);
                    selected_service_ids.insert(alternative.pattern_id.clone());
                    if network
                        .pattern(&alternative.pattern_id)
                        .is_some_and(|service| network.route_of(service).mode == TransitMode::Bus)
                    {
                        selected_bus_stop_ids.insert(leg.board_stop_id.clone());
                    }
                }
            }
        }

        let mut boardings_by_stop = HashMap::<BoardingPointId, Vec<CandidateBoarding>>::new();
        for (position, min_remaining_legs) in &minimums_by_position {
            let Some(service) = network.pattern(&position.pattern_id) else {
                continue;
            };
            let Some(stop_id) = service.stops.get(position.stop_index) else {
                continue;
            };
            boardings_by_stop
                .entry(stop_id.clone())
                .or_default()
                .push(CandidateBoarding {
                    pattern_id: position.pattern_id.clone(),
                    board_index: position.stop_index,
                    min_remaining_legs: *min_remaining_legs,
                });
        }
        for boardings in boardings_by_stop.values_mut() {
            boardings.sort_by(|left, right| {
                left.pattern_id
                    .as_str()
                    .cmp(right.pattern_id.as_str())
                    .then(left.board_index.cmp(&right.board_index))
            });
        }

        Self {
            retained,
            boardings_by_stop,
            minimums_by_position,
            selected_service_ids,
            selected_bus_stop_ids,
            terminal_transfers,
        }
    }

    pub fn retained(&self) -> &[StaticCorridor] {
        &self.retained
    }

    pub fn boardings_at(&self, stop: &BoardingPointId) -> &[CandidateBoarding] {
        self.boardings_by_stop
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn remaining_legs(
        &self,
        pattern_id: &TransitPatternId,
        board_index: usize,
    ) -> Option<usize> {
        self.minimums_by_position
            .get(&PatternPosition {
                pattern_id: pattern_id.clone(),
                stop_index: board_index,
            })
            .copied()
    }

    pub fn positions(&self) -> impl Iterator<Item = PatternPosition> + '_ {
        self.minimums_by_position.keys().cloned()
    }

    pub fn bus_stops(&self) -> Vec<BoardingPointId> {
        let mut stops = self
            .selected_bus_stop_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        stops.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        stops
    }

    pub fn len(&self) -> usize {
        self.minimums_by_position.len()
    }

    pub fn is_empty(&self) -> bool {
        self.retained.is_empty()
    }

    pub fn service_count(&self) -> usize {
        self.selected_service_ids.len()
    }

    pub fn allows_terminal_transfer(&self, from: &BoardingPointId, to: &BoardingPointId) -> bool {
        self.terminal_transfers
            .contains(&(from.clone(), to.clone()))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StaticSearchStats {
    pub static_search_states_expanded: usize,
    pub static_unattributed_states_expanded: usize,
    pub static_service_scans: usize,
    pub static_positions_scanned: usize,
    pub static_walking_transfer_expansions: usize,
    pub static_duplicate_states_rejected: usize,
    pub static_dominance_states_rejected: usize,
    pub static_structurally_equivalent_states_rejected: usize,
    pub static_equivalent_service_occurrences_collapsed: usize,
    pub static_required_endpoint_mode_pairs: Vec<(TransitMode, TransitMode)>,
    pub static_complete_endpoint_mode_pairs: Vec<(TransitMode, TransitMode)>,
    pub static_complete_candidates_found: usize,
    pub static_unique_corridors_found: usize,
    pub static_unique_structural_corridors_found: usize,
    pub static_corridors_retained: usize,
    pub static_retained_sequence_groups: Vec<RetainedSequenceStats>,
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
    pub static_corridor_services: usize,
    pub static_corridor_positions: usize,
    pub static_corridor_bus_stops: usize,
    pub static_search_latency_ms: u64,
    pub static_pattern_index_build_latency_us: u64,
    pub static_pattern_index_service_edge_scans: usize,
    pub static_pattern_index_positions_scanned: usize,
    pub static_ride_profile_build_latency_us: u64,
    pub static_ride_profile_services_scanned: usize,
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
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitModeSearchStats {
    pub states_expanded: usize,
    pub service_scans: usize,
    pub downstream_positions_scanned: usize,
    pub distinct_services: usize,
    pub distinct_patterns: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedSequenceStats {
    pub service_sequence: Vec<TransitPatternId>,
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

#[derive(Default)]
struct ModeSearchAccumulator {
    states_expanded: usize,
    service_scans: usize,
    downstream_positions_scanned: usize,
    services: HashSet<TransitPatternId>,
    patterns: HashSet<Vec<BoardingPointId>>,
}

#[derive(Clone, Debug)]
struct StaticState {
    stop_id: BoardingPointId,
    elapsed_seconds: u32,
    lower_bound_seconds: u32,
    priority_seconds: u32,
    access_stop_id: BoardingPointId,
    transit_legs_used: usize,
    last_transit_mode: Option<TransitMode>,
    mode_transition_sequence: Vec<TransitMode>,
    steps: Vec<StaticCorridorStep>,
    has_exact_cost: bool,
    has_derived_bus_cost: bool,
    has_estimated_cost: bool,
    transfer_ready: bool,
}

impl StaticState {
    fn signature(&self) -> PartialSignature {
        PartialSignature {
            access_stop_id: self.access_stop_id.clone(),
            steps: structural_steps(&self.steps),
        }
    }

    fn continuation_signature(&self) -> ContinuationSignature {
        ContinuationSignature {
            access_stop_id: self.access_stop_id.clone(),
            mode_transition_sequence: self.mode_transition_sequence.clone(),
        }
    }

    fn cost_source(&self) -> StaticCostSource {
        match (
            self.has_exact_cost,
            self.has_derived_bus_cost,
            self.has_estimated_cost,
        ) {
            (true, false, false) => StaticCostSource::ExactSchedule,
            (false, true, false) => StaticCostSource::DerivedBus,
            (false, false, true) | (false, false, false) => StaticCostSource::OptimisticEstimate,
            _ => StaticCostSource::Mixed,
        }
    }
}

#[derive(Clone, Debug)]
struct QueueEntry {
    state: StaticState,
    sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LowerBoundEntry {
    stop_id: BoardingPointId,
    seconds: u32,
    sequence: u64,
}

impl PartialOrd for LowerBoundEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LowerBoundEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .seconds
            .cmp(&self.seconds)
            .then(other.sequence.cmp(&self.sequence))
    }
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.state.priority_seconds == other.state.priority_seconds
            && self.state.lower_bound_seconds == other.state.lower_bound_seconds
            && self.state.elapsed_seconds == other.state.elapsed_seconds
            && self.sequence == other.sequence
    }
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .state
            .priority_seconds
            .cmp(&self.state.priority_seconds)
            .then(other.state.elapsed_seconds.cmp(&self.state.elapsed_seconds))
            .then(other.sequence.cmp(&self.sequence))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DominanceKey {
    stop_id: BoardingPointId,
    transit_legs_used: usize,
    transfer_ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct EndpointModePair {
    first: TransitMode,
    last: TransitMode,
}

#[derive(Clone, Debug)]
struct KnownState {
    elapsed_seconds: u32,
    signature: PartialSignature,
    continuation_signature: ContinuationSignature,
}

#[derive(Clone, Debug)]
struct PatternRideAlternative {
    leg: StaticTransitLeg,
    duration_seconds: u32,
}

#[derive(Clone, Debug)]
struct PatternRideOption {
    alight_index: usize,
    alight_stop_id: BoardingPointId,
    alternatives: Vec<PatternRideAlternative>,
}

#[derive(Clone, Debug)]
struct PatternRideCache {
    mode: TransitMode,
    pattern_id: TransitPatternId,
    stops: Vec<BoardingPointId>,
    segments: Vec<SegmentTravelTime>,
    rides_by_board_index: HashMap<usize, Vec<PatternRideOption>>,
}

#[derive(Clone, Debug, Default)]
struct StaticPhase1PatternIndex {
    groups: Vec<PatternRideCache>,
    group_index_by_pattern: HashMap<TransitPatternId, usize>,
}

impl StaticPhase1PatternIndex {
    fn new(
        network: &TransitNetwork,
        allowed_modes: &[TransitMode],
        stats: &mut StaticSearchStats,
    ) -> Self {
        let mut index = Self::default();
        for pattern in &network.patterns {
            let mode = network.route_of(pattern).mode;
            if !allowed_modes.contains(&mode) {
                continue;
            }
            let group_index = index.groups.len();
            index
                .group_index_by_pattern
                .insert(pattern.id.clone(), group_index);
            stats.static_pattern_index_service_edge_scans += pattern.static_profile.segments.len();
            stats.static_pattern_index_positions_scanned += pattern.static_profile.segments.len();
            index.groups.push(PatternRideCache {
                mode,
                pattern_id: pattern.id.clone(),
                stops: pattern.stops.clone(),
                segments: pattern.static_profile.segments.clone(),
                rides_by_board_index: HashMap::new(),
            });
        }
        index
    }

    fn group_index(&self, pattern_id: &TransitPatternId) -> Option<usize> {
        self.group_index_by_pattern.get(pattern_id).copied()
    }

    fn group(&self, group_index: usize) -> &PatternRideCache {
        &self.groups[group_index]
    }

    fn ensure_ride_options(
        &mut self,
        group_index: usize,
        board_index: usize,
        stats: &mut StaticSearchStats,
        equivalent_legs: &mut HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    ) -> bool {
        let group = &mut self.groups[group_index];
        if group.rides_by_board_index.contains_key(&board_index) {
            return false;
        }
        let mut options_by_alight = HashMap::<usize, Vec<PatternRideAlternative>>::new();
        {
            let pattern_id = &group.pattern_id;
            stats.static_ride_profile_services_scanned += 1;
            let mut duration_seconds = 0_u32;
            let mut first_source = None;
            let mut mixed_source = false;
            for alight_index in (board_index + 1)..group.stops.len() {
                stats.static_ride_profile_positions_scanned += 1;
                let Some(segment) = group.segments.get(alight_index - 1) else {
                    continue;
                };
                let Some(total) = duration_seconds.checked_add(segment.seconds) else {
                    break;
                };
                duration_seconds = total;
                mixed_source |= first_source.is_some_and(|first| first != segment.source);
                first_source.get_or_insert(segment.source);
                let cost_source = if mixed_source {
                    StaticCostSource::Mixed
                } else {
                    match segment.source {
                        SegmentTimeSource::Schedule => StaticCostSource::ExactSchedule,
                        SegmentTimeSource::Derived => StaticCostSource::DerivedBus,
                        SegmentTimeSource::Estimated => StaticCostSource::OptimisticEstimate,
                    }
                };
                let Some(alight_stop_id) = group.stops.get(alight_index).cloned() else {
                    continue;
                };
                let leg = StaticTransitLeg {
                    pattern_id: pattern_id.clone(),
                    board_stop_id: group.stops[board_index].clone(),
                    board_index,
                    alight_stop_id,
                    alight_index,
                    cost_source,
                };
                let key = EquivalentLegKey {
                    mode: group.mode,
                    board_stop_id: leg.board_stop_id.clone(),
                    alight_stop_id: leg.alight_stop_id.clone(),
                };
                let aliases = equivalent_legs.entry(key).or_default();
                if !aliases.iter().any(|known| {
                    known.pattern_id == leg.pattern_id
                        && known.board_index == leg.board_index
                        && known.alight_index == leg.alight_index
                }) {
                    aliases.push(leg.clone());
                }
                options_by_alight
                    .entry(alight_index)
                    .or_default()
                    .push(PatternRideAlternative {
                        leg,
                        duration_seconds,
                    });
            }
        }
        let mut options = options_by_alight
            .into_iter()
            .map(|(alight_index, mut alternatives)| {
                alternatives.sort_by(|left, right| {
                    left.duration_seconds.cmp(&right.duration_seconds).then(
                        left.leg
                            .pattern_id
                            .as_str()
                            .cmp(right.leg.pattern_id.as_str()),
                    )
                });
                PatternRideOption {
                    alight_index,
                    alight_stop_id: group.stops[alight_index].clone(),
                    alternatives,
                }
            })
            .collect::<Vec<_>>();
        options.sort_by_key(|option| option.alight_index);
        group.rides_by_board_index.insert(board_index, options);
        true
    }

    fn ride_options(&self, group_index: usize, board_index: usize) -> &[PatternRideOption] {
        self.groups[group_index]
            .rides_by_board_index
            .get(&board_index)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

pub fn generate(
    network: &TransitNetwork,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &TransitQuery,
) -> (CandidateCorridor, StaticSearchStats) {
    let started = Instant::now();
    let limit = query.max_static_corridors.max(1);
    let max_legs = query.max_transfers.saturating_add(1);
    let mut stats = StaticSearchStats::default();
    let pattern_index_started = Instant::now();
    let mut patterns =
        StaticPhase1PatternIndex::new(network, &query.allowed_transit_modes, &mut stats);
    stats.static_pattern_index_build_latency_us = elapsed_microseconds(pattern_index_started);
    let egress_by_stop = query.destination_egress.iter().fold(
        HashMap::<BoardingPointId, Vec<u32>>::new(),
        |mut by_stop, egress| {
            by_stop
                .entry(egress.stop_id.clone())
                .or_default()
                .push(egress.walking_seconds);
            by_stop
        },
    );
    let terminal_transfer_source_stops = egress_by_stop
        .keys()
        .flat_map(|egress_stop| transfers.transfers_to(egress_stop))
        .map(|shortcut| shortcut.from.clone())
        .collect::<HashSet<_>>();
    let lower_bound_started = Instant::now();
    let destination_lower_bounds =
        destination_lower_bounds(&patterns, topology, transfers, query, &mut stats);
    stats.static_lower_bound_latency_us = elapsed_microseconds(lower_bound_started);
    let required_mode_pairs = reachable_endpoint_mode_pairs(
        network,
        topology,
        transfers,
        query,
        &destination_lower_bounds,
    );
    stats.static_required_endpoint_mode_pairs = sorted_mode_pairs(&required_mode_pairs);
    let mut frontier = BinaryHeap::new();
    let mut known = HashMap::<DominanceKey, Vec<KnownState>>::new();
    let mut sequence = 0_u64;
    for access in &query.origin_access {
        let Some(remaining_seconds) = destination_lower_bounds.get(&access.stop_id).copied() else {
            continue;
        };
        let elapsed_seconds = access.walking_seconds;
        let state = StaticState {
            stop_id: access.stop_id.clone(),
            elapsed_seconds,
            lower_bound_seconds: elapsed_seconds.saturating_add(remaining_seconds),
            priority_seconds: elapsed_seconds.saturating_add(remaining_seconds),
            access_stop_id: access.stop_id.clone(),
            transit_legs_used: 0,
            last_transit_mode: None,
            mode_transition_sequence: Vec::new(),
            steps: Vec::new(),
            has_exact_cost: false,
            has_derived_bus_cost: false,
            has_estimated_cost: false,
            transfer_ready: false,
        };
        enqueue_state(
            &mut frontier,
            &mut known,
            state,
            limit,
            &mut sequence,
            &mut stats,
        );
    }

    let forward_search_started = Instant::now();
    let mut retained = HashMap::<CorridorSignature, StaticCorridor>::new();
    let mut best_by_mode_transition_sequence = HashMap::<Vec<TransitMode>, StaticCorridor>::new();
    let mut all_unique = HashSet::new();
    let mut all_structurally_unique = HashSet::new();
    let mut complete_mode_pairs = HashSet::<EndpointModePair>::new();
    let mut equivalent_legs = HashMap::<EquivalentLegKey, Vec<StaticTransitLeg>>::new();
    let mut expanded_states_by_leg_count = HashMap::<usize, usize>::new();
    let mut expanded_stops = HashMap::<BoardingPointId, usize>::new();
    let requires_endpoint_pair_coverage = query.allowed_transit_modes.len() <= 2;
    let mut mode_accumulators = HashMap::<TransitMode, ModeSearchAccumulator>::new();
    while let Some(entry) = frontier.pop() {
        if kth_duration(&retained, limit).is_some_and(|kth| {
            let has_multi_mode_family = best_by_mode_transition_sequence
                .keys()
                .any(|mode_sequence| mode_sequence.len() > 1);
            let structural_coverage_complete = if requires_endpoint_pair_coverage {
                complete_mode_pairs.is_superset(&required_mode_pairs)
            } else {
                has_multi_mode_family
            };
            structural_coverage_complete
                && entry.state.lower_bound_seconds >= kth
                && frontier
                    .iter()
                    .all(|queued| queued.state.lower_bound_seconds >= kth)
        }) {
            break;
        }
        if !is_current(&known, &entry.state) {
            continue;
        }
        if stats.static_search_states_expanded >= query.max_static_search_expansions {
            stats.static_search_limit_reached = true;
            break;
        }
        stats.static_search_states_expanded += 1;
        let state = entry.state;
        *expanded_states_by_leg_count
            .entry(state.transit_legs_used)
            .or_default() += 1;
        *expanded_stops.entry(state.stop_id.clone()).or_default() += 1;
        if egress_by_stop.contains_key(&state.stop_id) {
            stats.static_expanded_egress_states += 1;
        }
        if terminal_transfer_source_stops.contains(&state.stop_id) {
            stats.static_expanded_terminal_transfer_source_states += 1;
        }
        if let Some(mode) = state.last_transit_mode {
            mode_accumulators.entry(mode).or_default().states_expanded += 1;
        } else {
            stats.static_unattributed_states_expanded += 1;
        }
        let occurrences = topology.occurrences_at(&state.stop_id);
        let pattern_occurrences = occurrences
            .iter()
            .filter(|position| patterns.group_index(&position.pattern_id).is_some())
            .cloned()
            .collect::<Vec<_>>();
        stats.static_expanded_stop_occurrence_samples += 1;
        stats.static_expanded_stop_occurrences_total += occurrences.len();
        stats.static_expanded_stop_pattern_occurrences_total += pattern_occurrences.len();
        stats.static_expanded_stop_occurrences_max = stats
            .static_expanded_stop_occurrences_max
            .max(occurrences.len());
        stats.static_expanded_stop_pattern_occurrences_max = stats
            .static_expanded_stop_pattern_occurrences_max
            .max(pattern_occurrences.len());

        if !state.transfer_ready {
            for shortcut in transfers.transfers_from(&state.stop_id) {
                stats.static_walking_transfer_expansions += 1;
                let elapsed_seconds = state
                    .elapsed_seconds
                    .saturating_add(shortcut.duration_seconds);
                if elapsed_seconds > query.max_journey_duration_seconds {
                    continue;
                }
                let mut next = state.clone();
                next.stop_id = shortcut.to.clone();
                next.elapsed_seconds = elapsed_seconds;
                let Some(remaining_seconds) = destination_lower_bounds.get(&shortcut.to).copied()
                else {
                    continue;
                };
                next.lower_bound_seconds = elapsed_seconds.saturating_add(remaining_seconds);
                next.priority_seconds = next.lower_bound_seconds;
                next.steps.push(StaticCorridorStep::WalkingTransfer {
                    from: shortcut.from.clone(),
                    to: shortcut.to.clone(),
                    duration_seconds: shortcut.duration_seconds,
                });
                next.transfer_ready = true;
                if next.transit_legs_used > 0 {
                    record_complete_corridors(
                        &next,
                        &egress_by_stop,
                        query.max_journey_duration_seconds,
                        limit,
                        &mut stats,
                        &mut retained,
                        &mut all_unique,
                        &mut all_structurally_unique,
                        &mut best_by_mode_transition_sequence,
                        &mut complete_mode_pairs,
                        network,
                    );
                }
                if next.transit_legs_used < max_legs {
                    enqueue_state(
                        &mut frontier,
                        &mut known,
                        next,
                        limit,
                        &mut sequence,
                        &mut stats,
                    );
                }
            }
        }

        if state.transit_legs_used >= max_legs {
            continue;
        }

        for position in pattern_occurrences {
            let group_index = patterns
                .group_index(&position.pattern_id)
                .expect("indexed pattern");
            let group = patterns.group(group_index);
            let mode = group.mode;
            if !query.allowed_transit_modes.contains(&mode) {
                continue;
            }
            stats.static_service_scans += 1;
            {
                let mode_stats = mode_accumulators.entry(mode).or_default();
                mode_stats.service_scans += 1;
                mode_stats.services.insert(group.pattern_id.clone());
                mode_stats.patterns.insert(group.stops.clone());
            }
            let profile_started = Instant::now();
            if patterns.ensure_ride_options(
                group_index,
                position.stop_index,
                &mut stats,
                &mut equivalent_legs,
            ) {
                stats.static_ride_profile_build_latency_us = stats
                    .static_ride_profile_build_latency_us
                    .saturating_add(elapsed_microseconds(profile_started));
            }
            let transfer_seconds = if state.transit_legs_used > 0 && !state.transfer_ready {
                network.default_transfer_seconds
            } else {
                0
            };
            for ride_option in patterns.ride_options(group_index, position.stop_index) {
                stats.static_positions_scanned += 1;
                mode_accumulators
                    .entry(mode)
                    .or_default()
                    .downstream_positions_scanned += 1;
                let Some(alternative) = ride_option.alternatives.first() else {
                    continue;
                };
                stats.static_structurally_equivalent_states_rejected +=
                    ride_option.alternatives.len().saturating_sub(1);
                let ride_seconds = alternative.duration_seconds;
                let source = alternative.leg.cost_source;
                let elapsed_seconds = state
                    .elapsed_seconds
                    .saturating_add(transfer_seconds)
                    .saturating_add(ride_seconds);
                if elapsed_seconds > query.max_journey_duration_seconds {
                    continue;
                }
                let alight_stop_id = ride_option.alight_stop_id.clone();
                let mut next = state.clone();
                next.stop_id = alight_stop_id.clone();
                next.elapsed_seconds = elapsed_seconds;
                let Some(remaining_seconds) =
                    destination_lower_bounds.get(&alight_stop_id).copied()
                else {
                    continue;
                };
                next.lower_bound_seconds = elapsed_seconds.saturating_add(remaining_seconds);
                next.priority_seconds = next.lower_bound_seconds;
                next.transit_legs_used += 1;
                if state.last_transit_mode != Some(mode) {
                    next.mode_transition_sequence.push(mode);
                }
                next.last_transit_mode = Some(mode);
                next.steps
                    .push(StaticCorridorStep::Transit(alternative.leg.clone()));
                next.has_exact_cost |= source == StaticCostSource::ExactSchedule;
                next.has_derived_bus_cost |= source == StaticCostSource::DerivedBus;
                next.has_estimated_cost |= source == StaticCostSource::OptimisticEstimate;
                if source == StaticCostSource::Mixed {
                    next.has_exact_cost = true;
                    next.has_estimated_cost = true;
                }
                next.transfer_ready = false;
                record_complete_corridors(
                    &next,
                    &egress_by_stop,
                    query.max_journey_duration_seconds,
                    limit,
                    &mut stats,
                    &mut retained,
                    &mut all_unique,
                    &mut all_structurally_unique,
                    &mut best_by_mode_transition_sequence,
                    &mut complete_mode_pairs,
                    network,
                );
                enqueue_state(
                    &mut frontier,
                    &mut known,
                    next,
                    limit,
                    &mut sequence,
                    &mut stats,
                );
            }
        }
    }

    stats.static_forward_search_latency_us = elapsed_microseconds(forward_search_started);
    stats.static_expanded_stop_occurrences_average_milli = stats
        .static_expanded_stop_occurrences_total
        .saturating_mul(1_000)
        .checked_div(stats.static_expanded_stop_occurrence_samples)
        .unwrap_or_default();
    stats.static_expanded_stop_pattern_occurrences_average_milli = stats
        .static_expanded_stop_pattern_occurrences_total
        .saturating_mul(1_000)
        .checked_div(stats.static_expanded_stop_occurrence_samples)
        .unwrap_or_default();
    stats.static_mode_stats = mode_accumulators
        .into_iter()
        .map(|(mode, accumulator)| {
            (
                mode,
                TransitModeSearchStats {
                    states_expanded: accumulator.states_expanded,
                    service_scans: accumulator.service_scans,
                    downstream_positions_scanned: accumulator.downstream_positions_scanned,
                    distinct_services: accumulator.services.len(),
                    distinct_patterns: accumulator.patterns.len(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    stats.static_expanded_states_by_leg_count = expanded_states_by_leg_count
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect();
    stats.static_most_expanded_stops = expanded_stops.into_iter().collect();
    stats
        .static_most_expanded_stops
        .sort_by(|(left_id, left_count), (right_id, right_count)| {
            right_count
                .cmp(left_count)
                .then(left_id.as_str().cmp(right_id.as_str()))
        });
    stats.static_most_expanded_stops.truncate(12);

    let retention_started = Instant::now();
    stats.static_complete_endpoint_mode_pairs = sorted_mode_pairs(&complete_mode_pairs);
    let mut global_candidates = retained.into_values().collect::<Vec<_>>();
    global_candidates.sort_by(corridor_order);
    let mut structural_families = best_by_mode_transition_sequence
        .into_values()
        .collect::<Vec<_>>();
    structural_families.sort_by(corridor_order);
    let mut selected_structures = HashSet::new();
    let mut retained = Vec::new();
    for corridor in structural_families.into_iter().chain(global_candidates) {
        if retained.len() >= limit {
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
            retained.push(corridor);
        }
    }
    retained.sort_by(corridor_order);
    let surface = CandidateCorridor::from_retained(network, &equivalent_legs, retained);
    stats.static_retained_sequence_groups = retained_sequence_groups(network, surface.retained());
    stats.static_retained_mode_sequence_groups =
        retained_mode_sequence_groups(&stats.static_retained_sequence_groups);
    stats.static_corridors_retained = surface.retained().len();
    stats.static_corridor_services = surface.service_count();
    stats.static_corridor_positions = surface.len();
    stats.static_corridor_bus_stops = surface.bus_stops().len();
    stats.kth_static_duration_seconds = surface
        .retained()
        .get(limit.saturating_sub(1))
        .map(|corridor| corridor.theoretical_duration_seconds);
    for corridor in surface.retained() {
        match corridor.cost_source {
            StaticCostSource::ExactSchedule => stats.exact_schedule_cost += 1,
            StaticCostSource::DerivedBus => stats.derived_bus_cost += 1,
            StaticCostSource::OptimisticEstimate => stats.estimated_cost += 1,
            StaticCostSource::Mixed => stats.mixed_cost += 1,
        }
    }
    stats.static_retention_latency_us = elapsed_microseconds(retention_started);
    stats.static_search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    (surface, stats)
}

fn elapsed_microseconds(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn retained_sequence_groups(
    network: &TransitNetwork,
    corridors: &[StaticCorridor],
) -> Vec<RetainedSequenceStats> {
    let mut groups =
        HashMap::<(Vec<TransitPatternId>, Vec<TransitMode>), RetainedSequenceStats>::new();
    for corridor in corridors {
        let service_sequence = corridor.service_sequence();
        let mode_sequence = service_sequence
            .iter()
            .filter_map(|pattern_id| {
                network
                    .pattern(pattern_id)
                    .map(|service| network.route_of(service).mode)
            })
            .collect::<Vec<_>>();
        let key = (service_sequence.clone(), mode_sequence.clone());
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
            .or_insert_with(|| RetainedSequenceStats {
                service_sequence,
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
                left.service_sequence
                    .len()
                    .cmp(&right.service_sequence.len()),
            )
            .then_with(|| {
                left.service_sequence
                    .iter()
                    .map(TransitPatternId::as_str)
                    .cmp(right.service_sequence.iter().map(TransitPatternId::as_str))
            })
    });
    groups
}

fn retained_mode_sequence_groups(
    service_groups: &[RetainedSequenceStats],
) -> Vec<RetainedModeSequenceStats> {
    let mut groups = HashMap::<Vec<TransitMode>, RetainedModeSequenceStats>::new();
    for service_group in service_groups {
        groups
            .entry(service_group.mode_sequence.clone())
            .and_modify(|group| {
                group.corridors += service_group.corridors;
                group.best_duration_seconds = group
                    .best_duration_seconds
                    .min(service_group.best_duration_seconds);
                group.worst_duration_seconds = group
                    .worst_duration_seconds
                    .max(service_group.worst_duration_seconds);
            })
            .or_insert_with(|| RetainedModeSequenceStats {
                mode_sequence: service_group.mode_sequence.clone(),
                corridors: service_group.corridors,
                best_duration_seconds: service_group.best_duration_seconds,
                worst_duration_seconds: service_group.worst_duration_seconds,
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

#[allow(clippy::too_many_arguments)]
fn record_complete_corridors(
    state: &StaticState,
    egress_by_stop: &HashMap<BoardingPointId, Vec<u32>>,
    max_journey_duration_seconds: u32,
    limit: usize,
    stats: &mut StaticSearchStats,
    retained: &mut HashMap<CorridorSignature, StaticCorridor>,
    all_unique: &mut HashSet<CorridorSignature>,
    all_structurally_unique: &mut HashSet<EquivalentCorridorSignature>,
    best_by_mode_transition_sequence: &mut HashMap<Vec<TransitMode>, StaticCorridor>,
    complete_mode_pairs: &mut HashSet<EndpointModePair>,
    network: &TransitNetwork,
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
        if all_unique.insert(signature.clone()) {
            stats.static_unique_corridors_found += 1;
        }
        if let Some(steps) = equivalent_steps(network, &corridor.steps) {
            let structural_signature = EquivalentCorridorSignature {
                access_stop_id: corridor.access_stop_id.clone(),
                steps,
                egress_stop_id: corridor.egress_stop_id.clone(),
            };
            if all_structurally_unique.insert(structural_signature) {
                stats.static_unique_structural_corridors_found += 1;
            }
        }
        let modes = corridor
            .transit_legs()
            .filter_map(|leg| {
                network
                    .pattern(&leg.pattern_id)
                    .map(|service| network.route_of(service).mode)
            })
            .collect::<Vec<_>>();
        if let (Some(first), Some(last)) = (modes.first(), modes.last()) {
            complete_mode_pairs.insert(EndpointModePair {
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
        let family_replace = best_by_mode_transition_sequence
            .get(&transition_sequence)
            .is_none_or(|known| corridor_order(&corridor, known).is_lt());
        if family_replace {
            best_by_mode_transition_sequence.insert(transition_sequence, corridor.clone());
        }
        let replace = retained.get(&signature).is_none_or(|known| {
            corridor.theoretical_duration_seconds < known.theoretical_duration_seconds
        });
        if replace {
            retained.insert(signature, corridor);
            trim_retained(retained, limit);
        }
    }
}

fn enqueue_state(
    frontier: &mut BinaryHeap<QueueEntry>,
    known: &mut HashMap<DominanceKey, Vec<KnownState>>,
    state: StaticState,
    limit: usize,
    sequence: &mut u64,
    stats: &mut StaticSearchStats,
) {
    let key = DominanceKey {
        stop_id: state.stop_id.clone(),
        transit_legs_used: state.transit_legs_used,
        transfer_ready: state.transfer_ready,
    };
    let signature = state.signature();
    let continuation_signature = state.continuation_signature();
    let alternatives = known.entry(key).or_default();
    if alternatives
        .iter()
        .any(|known| known.signature == signature && known.elapsed_seconds <= state.elapsed_seconds)
    {
        stats.static_duplicate_states_rejected += 1;
        return;
    }
    if alternatives.iter().any(|known| {
        known.continuation_signature == continuation_signature
            && known.elapsed_seconds <= state.elapsed_seconds
    }) {
        stats.static_structurally_equivalent_states_rejected += 1;
        return;
    }
    if alternatives.len() >= limit
        && alternatives
            .iter()
            .filter(|known| known.elapsed_seconds <= state.elapsed_seconds)
            .count()
            >= limit
    {
        stats.static_dominance_states_rejected += 1;
        return;
    }
    alternatives.retain(|known| {
        known.signature != signature && known.continuation_signature != continuation_signature
    });
    alternatives.push(KnownState {
        elapsed_seconds: state.elapsed_seconds,
        signature,
        continuation_signature,
    });
    alternatives.sort_by_key(|known| known.elapsed_seconds);
    alternatives.truncate(limit);
    frontier.push(QueueEntry {
        state,
        sequence: *sequence,
    });
    *sequence = sequence.saturating_add(1);
}

fn is_current(known: &HashMap<DominanceKey, Vec<KnownState>>, state: &StaticState) -> bool {
    let key = DominanceKey {
        stop_id: state.stop_id.clone(),
        transit_legs_used: state.transit_legs_used,
        transfer_ready: state.transfer_ready,
    };
    let signature = state.signature();
    known.get(&key).is_some_and(|alternatives| {
        alternatives.iter().any(|known| {
            known.signature == signature && known.elapsed_seconds == state.elapsed_seconds
        })
    })
}

fn destination_lower_bounds(
    patterns: &StaticPhase1PatternIndex,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &TransitQuery,
    stats: &mut StaticSearchStats,
) -> HashMap<BoardingPointId, u32> {
    let mut lower_bounds = HashMap::<BoardingPointId, u32>::new();
    let mut frontier = BinaryHeap::new();
    let mut sequence = 0_u64;
    for egress in &query.destination_egress {
        if egress.walking_seconds > query.max_journey_duration_seconds {
            continue;
        }
        if lower_bounds
            .get(&egress.stop_id)
            .is_some_and(|known| *known <= egress.walking_seconds)
        {
            continue;
        }
        lower_bounds.insert(egress.stop_id.clone(), egress.walking_seconds);
        frontier.push(LowerBoundEntry {
            stop_id: egress.stop_id.clone(),
            seconds: egress.walking_seconds,
            sequence,
        });
        sequence = sequence.saturating_add(1);
    }

    while let Some(entry) = frontier.pop() {
        if lower_bounds.get(&entry.stop_id) != Some(&entry.seconds) {
            continue;
        }
        stats.static_lower_bound_nodes_settled += 1;
        for position in topology.occurrences_at(&entry.stop_id) {
            stats.static_lower_bound_pattern_edges_scanned += 1;
            let Some(predecessor_index) = position.stop_index.checked_sub(1) else {
                continue;
            };
            let Some(group_index) = patterns.group_index(&position.pattern_id) else {
                continue;
            };
            let group = patterns.group(group_index);
            let Some(predecessor) = group.stops.get(predecessor_index) else {
                continue;
            };
            let Some(edge_seconds) = group
                .segments
                .get(predecessor_index)
                .map(|segment| segment.seconds)
            else {
                continue;
            };
            relax_lower_bound(
                predecessor,
                entry.seconds.saturating_add(edge_seconds),
                &mut lower_bounds,
                &mut frontier,
                &mut sequence,
                query.max_journey_duration_seconds,
            );
        }
        for shortcut in transfers.transfers_to(&entry.stop_id) {
            stats.static_lower_bound_transfer_edges_scanned += 1;
            relax_lower_bound(
                &shortcut.from,
                entry.seconds.saturating_add(shortcut.duration_seconds),
                &mut lower_bounds,
                &mut frontier,
                &mut sequence,
                query.max_journey_duration_seconds,
            );
        }
    }
    lower_bounds
}

fn relax_lower_bound(
    stop_id: &BoardingPointId,
    seconds: u32,
    lower_bounds: &mut HashMap<BoardingPointId, u32>,
    frontier: &mut BinaryHeap<LowerBoundEntry>,
    sequence: &mut u64,
    maximum_seconds: u32,
) {
    if seconds > maximum_seconds {
        return;
    }
    if lower_bounds
        .get(stop_id)
        .is_some_and(|known| *known <= seconds)
    {
        return;
    }
    lower_bounds.insert(stop_id.clone(), seconds);
    frontier.push(LowerBoundEntry {
        stop_id: stop_id.clone(),
        seconds,
        sequence: *sequence,
    });
    *sequence = sequence.saturating_add(1);
}

fn kth_duration(
    retained: &HashMap<CorridorSignature, StaticCorridor>,
    limit: usize,
) -> Option<u32> {
    if retained.len() < limit {
        return None;
    }
    retained
        .values()
        .map(|corridor| corridor.theoretical_duration_seconds)
        .max()
}

fn trim_retained(retained: &mut HashMap<CorridorSignature, StaticCorridor>, limit: usize) {
    if retained.len() <= limit {
        return;
    }
    let worst = retained
        .iter()
        .max_by(|(_, left), (_, right)| corridor_order(left, right))
        .map(|(signature, _)| signature.clone());
    if let Some(worst) = worst {
        retained.remove(&worst);
    }
}

fn reachable_endpoint_mode_pairs(
    network: &TransitNetwork,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    query: &TransitQuery,
    destination_lower_bounds: &HashMap<BoardingPointId, u32>,
) -> HashSet<EndpointModePair> {
    let mut origin_modes = HashSet::new();
    for access in &query.origin_access {
        let mut boarding_stops = vec![access.stop_id.clone()];
        boarding_stops.extend(
            transfers
                .transfers_from(&access.stop_id)
                .iter()
                .map(|shortcut| shortcut.to.clone()),
        );
        for stop_id in boarding_stops {
            if !destination_lower_bounds.contains_key(&stop_id) {
                continue;
            }
            for position in topology.occurrences_at(&stop_id) {
                let Some(service) = network.pattern(&position.pattern_id) else {
                    continue;
                };
                if query
                    .allowed_transit_modes
                    .contains(&network.route_of(service).mode)
                {
                    origin_modes.insert(network.route_of(service).mode);
                }
            }
        }
    }

    let mut destination_modes = HashSet::new();
    for egress in &query.destination_egress {
        let mut alighting_stops = vec![egress.stop_id.clone()];
        alighting_stops.extend(
            transfers
                .transfers_to(&egress.stop_id)
                .iter()
                .map(|shortcut| shortcut.from.clone()),
        );
        for stop_id in alighting_stops {
            for position in topology.occurrences_at(&stop_id) {
                let Some(service) = network.pattern(&position.pattern_id) else {
                    continue;
                };
                if query
                    .allowed_transit_modes
                    .contains(&network.route_of(service).mode)
                {
                    destination_modes.insert(network.route_of(service).mode);
                }
            }
        }
    }

    origin_modes
        .into_iter()
        .flat_map(|first| {
            destination_modes
                .iter()
                .copied()
                .map(move |last| EndpointModePair { first, last })
        })
        .collect()
}

fn sorted_mode_pairs(pairs: &HashSet<EndpointModePair>) -> Vec<(TransitMode, TransitMode)> {
    let mut pairs = pairs
        .iter()
        .map(|pair| (pair.first, pair.last))
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
}

fn corridor_order(left: &StaticCorridor, right: &StaticCorridor) -> Ordering {
    left.theoretical_duration_seconds
        .cmp(&right.theoretical_duration_seconds)
        .then_with(|| format!("{:?}", left.signature()).cmp(&format!("{:?}", right.signature())))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use time::{Time, Weekday, macros::datetime};

    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::{
            ids::TransitTripId,
            transit::{ScheduledStopTime, TransferEdge, TransitPattern, TransitStop, TransitTrip},
        },
        routing::{AccessPoint, EgressPoint},
    };

    fn id(value: &str) -> BoardingPointId {
        BoardingPointId::new(value)
    }

    fn service(value: &str, mode: TransitMode, pattern: &[&str]) -> TransitPattern {
        TransitPattern::fixture(
            TransitPatternId::new(value),
            mode,
            value,
            pattern.last().copied().unwrap_or_default(),
            pattern.iter().map(|stop| id(stop)).collect(),
        )
    }

    fn trip(value: &str, pattern_id: &str, offsets: &[u32]) -> TransitTrip {
        TransitTrip::fixture_weekdays(
            TransitTripId::new(value),
            TransitPatternId::new(pattern_id),
            vec![
                Weekday::Monday,
                Weekday::Tuesday,
                Weekday::Wednesday,
                Weekday::Thursday,
                Weekday::Friday,
                Weekday::Saturday,
                Weekday::Sunday,
            ],
            offsets
                .iter()
                .map(|seconds| ScheduledStopTime::new(*seconds, *seconds))
                .collect(),
        )
    }

    fn network(services: Vec<TransitPattern>, trips: Vec<TransitTrip>) -> TransitNetwork {
        let stop_ids = services
            .iter()
            .flat_map(|service| service.stops.iter().cloned())
            .collect::<HashSet<_>>();
        TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            stop_ids
                .into_iter()
                .enumerate()
                .map(|(index, stop_id)| {
                    TransitStop::new(
                        stop_id.clone(),
                        stop_id.as_str(),
                        RequestCoordinate {
                            latitude: 25.0 + index as f64 * 0.00001,
                            longitude: 121.5,
                        },
                    )
                })
                .collect(),
            services,
            trips,
        )
    }

    fn query(
        origin: &str,
        destination: &str,
        modes: &[TransitMode],
        max_transfers: usize,
        k: usize,
    ) -> TransitQuery {
        TransitQuery::new(
            vec![AccessPoint::new(
                id(origin),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(id(destination), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes(modes.iter().copied())
        .with_limits(max_transfers, 16, 2_000, 3, 4 * 60 * 60)
        .with_max_static_corridors(k)
    }

    fn run(network: &TransitNetwork, query: &TransitQuery) -> CandidateCorridor {
        let topology = TransitTopologyIndex::new(network);
        let transfers = TransferShortcutIndex::from_network(network);
        generate(network, &topology, &transfers, query).0
    }

    #[test]
    fn one_global_k_is_shared_across_bus_and_metro() {
        let network = network(
            vec![
                service("bus", TransitMode::Bus, &["o", "d"]),
                service("metro", TransitMode::Metro, &["o", "d"]),
            ],
            vec![
                trip("bus-trip", "bus", &[0, 100]),
                trip("metro-trip", "metro", &[0, 200]),
            ],
        );

        let surface = run(
            &network,
            &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 0, 1),
        );

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![TransitPatternId::new("bus")]
        );
    }

    #[test]
    fn mixed_bus_to_metro_can_outrank_slower_pure_bus() {
        let network = network(
            vec![
                service("connector", TransitMode::Bus, &["o", "hub"]),
                service("metro", TransitMode::Metro, &["hub", "d"]),
                service("slow-bus", TransitMode::Bus, &["o", "d"]),
            ],
            vec![
                trip("metro-trip", "metro", &[0, 300]),
                trip("slow-trip", "slow-bus", &[0, 1_200]),
            ],
        );

        let surface = run(
            &network,
            &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 1, 1),
        );

        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![
                TransitPatternId::new("connector"),
                TransitPatternId::new("metro")
            ]
        );
        assert_eq!(surface.retained()[0].cost_source, StaticCostSource::Mixed);
    }

    #[test]
    fn k_two_means_two_total_corridors_not_two_per_mode() {
        let network = network(
            vec![
                service("bus-a", TransitMode::Bus, &["o", "d"]),
                service("bus-b", TransitMode::Bus, &["o", "d"]),
                service("metro-a", TransitMode::Metro, &["o", "d"]),
                service("metro-b", TransitMode::Metro, &["o", "d"]),
            ],
            vec![
                trip("a", "bus-a", &[0, 100]),
                trip("b", "bus-b", &[0, 110]),
                trip("c", "metro-a", &[0, 120]),
                trip("d", "metro-b", &[0, 130]),
            ],
        );

        let surface = run(
            &network,
            &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 0, 2),
        );

        assert_eq!(surface.retained().len(), 2);
        assert_eq!(surface.service_count(), 4);
    }

    #[test]
    fn equivalent_services_share_one_pattern_scan_and_remain_in_the_candidate_union() {
        let network = network(
            vec![
                service("bus-a", TransitMode::Bus, &["o", "d"]),
                service("bus-b", TransitMode::Bus, &["o", "d"]),
            ],
            vec![trip("a", "bus-a", &[0, 100]), trip("b", "bus-b", &[0, 110])],
        );
        let topology = TransitTopologyIndex::new(&network);
        let transfers = TransferShortcutIndex::from_network(&network);

        let (surface, stats) = generate(
            &network,
            &topology,
            &transfers,
            &query("o", "d", &[TransitMode::Bus], 0, 1),
        );

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(surface.service_count(), 2);
        assert!(
            surface
                .remaining_legs(&TransitPatternId::new("bus-a"), 0)
                .is_some()
        );
        assert!(
            surface
                .remaining_legs(&TransitPatternId::new("bus-b"), 0)
                .is_some()
        );
        assert_eq!(
            stats.static_mode_stats[&TransitMode::Bus].distinct_services,
            2
        );
        assert_eq!(
            stats.static_mode_stats[&TransitMode::Bus].distinct_patterns,
            1
        );
        assert!(stats.static_structurally_equivalent_states_rejected > 0);
    }

    #[test]
    fn retained_segment_family_keeps_services_from_overlapping_patterns() {
        let network = network(
            vec![
                service("direct", TransitMode::Bus, &["o", "d"]),
                service(
                    "continues-past-destination",
                    TransitMode::Bus,
                    &["o", "d", "x"],
                ),
            ],
            vec![
                trip("direct-trip", "direct", &[0, 100]),
                trip("long-trip", "continues-past-destination", &[0, 110, 220]),
            ],
        );
        let topology = TransitTopologyIndex::new(&network);
        let transfers = TransferShortcutIndex::from_network(&network);

        let (surface, _stats) = generate(
            &network,
            &topology,
            &transfers,
            &query("o", "d", &[TransitMode::Bus], 0, 1),
        );

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(surface.service_count(), 2);
        assert!(
            surface
                .remaining_legs(&TransitPatternId::new("direct"), 0)
                .is_some()
        );
        assert!(
            surface
                .remaining_legs(&TransitPatternId::new("continues-past-destination"), 0)
                .is_some()
        );
    }

    #[test]
    fn retained_mode_transition_families_survive_a_bus_dominated_global_top_k() {
        let network = network(
            vec![
                service("bus-direct-a", TransitMode::Bus, &["o", "a", "d"]),
                service("bus-direct-b", TransitMode::Bus, &["o", "b", "d"]),
                service("metro", TransitMode::Metro, &["o", "m"]),
                service("bus-after-metro", TransitMode::Bus, &["x", "d"]),
            ],
            vec![
                trip("a", "bus-direct-a", &[0, 40, 100]),
                trip("b", "bus-direct-b", &[0, 45, 110]),
                trip("m", "metro", &[0, 80]),
                trip("last", "bus-after-metro", &[0, 80]),
            ],
        )
        .with_transfer_edges(vec![TransferEdge::new(id("m"), id("x"), 30)]);

        let surface = run(
            &network,
            &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 1, 2),
        );

        let mode_sequences = surface
            .retained()
            .iter()
            .map(|corridor| {
                corridor
                    .transit_legs()
                    .filter_map(|leg| network.pattern(&leg.pattern_id))
                    .map(|service| network.route_of(service).mode)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert!(mode_sequences.contains(&vec![TransitMode::Bus]));
        assert!(mode_sequences.contains(&vec![TransitMode::Metro, TransitMode::Bus]));
    }

    #[test]
    fn multiple_departures_of_one_structure_consume_one_slot() {
        let network = network(
            vec![service("bus", TransitMode::Bus, &["o", "d"])],
            vec![
                trip("morning", "bus", &[0, 100]),
                trip("evening", "bus", &[10_000, 10_100]),
            ],
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 100));

        assert_eq!(surface.retained().len(), 1);
    }

    #[test]
    fn much_slower_detour_is_excluded_when_k_is_full() {
        let network = network(
            vec![
                service("direct", TransitMode::Bus, &["o", "d"]),
                service("detour", TransitMode::Bus, &["o", "far", "d"]),
            ],
            vec![
                trip("direct-trip", "direct", &[0, 100]),
                trip("detour-trip", "detour", &[0, 1_800, 3_600]),
            ],
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 1));

        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![TransitPatternId::new("direct")]
        );
    }

    #[test]
    fn bus_without_exact_schedule_uses_optimistic_geometry() {
        let network = network(
            vec![service("realtime-only", TransitMode::Bus, &["o", "d"])],
            Vec::new(),
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 1));

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(
            surface.retained()[0].cost_source,
            StaticCostSource::OptimisticEstimate
        );
    }

    #[test]
    fn selected_mode_filtering_is_applied_before_retention() {
        let network = network(
            vec![
                service("bus", TransitMode::Bus, &["o", "d"]),
                service("metro", TransitMode::Metro, &["o", "d"]),
            ],
            vec![
                trip("bus-trip", "bus", &[0, 50]),
                trip("metro-trip", "metro", &[0, 100]),
            ],
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Metro], 0, 10));

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![TransitPatternId::new("metro")]
        );
    }

    #[test]
    fn maximum_transfer_count_is_respected() {
        let network = network(
            vec![
                service("first", TransitMode::Bus, &["o", "hub"]),
                service("second", TransitMode::Bus, &["hub", "d"]),
            ],
            vec![
                trip("first-trip", "first", &[0, 100]),
                trip("second-trip", "second", &[0, 100]),
            ],
        );

        let blocked = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 10));
        let allowed = run(&network, &query("o", "d", &[TransitMode::Bus], 1, 10));

        assert!(blocked.is_empty());
        assert_eq!(allowed.retained().len(), 1);
        assert_eq!(allowed.retained()[0].service_sequence().len(), 2);
    }

    #[test]
    fn static_walking_transfer_can_connect_access_to_first_boarding() {
        let network = network(
            vec![service("metro", TransitMode::Metro, &["board", "d"])],
            vec![trip("metro-trip", "metro", &[0, 100])],
        )
        .with_transfer_edges(vec![TransferEdge::new(id("access"), id("board"), 30)]);
        let mut network = network;
        network.stops.push(TransitStop::new(
            id("access"),
            "access",
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        ));

        let surface = run(
            &network,
            &query("access", "d", &[TransitMode::Metro], 0, 10),
        );

        assert_eq!(surface.retained().len(), 1);
        assert!(matches!(
            surface.retained()[0].steps.first(),
            Some(StaticCorridorStep::WalkingTransfer { .. })
        ));
    }

    #[test]
    fn static_walking_transfer_can_connect_final_alight_to_egress() {
        let network = network(
            vec![service("bus", TransitMode::Bus, &["o", "alight"])],
            vec![trip("bus-trip", "bus", &[0, 100])],
        )
        .with_transfer_edges(vec![TransferEdge::new(id("alight"), id("d"), 30)]);
        let mut network = network;
        network.stops.push(TransitStop::new(
            id("d"),
            "d",
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        ));

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 10));

        assert_eq!(surface.retained().len(), 1);
        assert!(matches!(
            surface.retained()[0].steps.last(),
            Some(StaticCorridorStep::WalkingTransfer { from, to, .. })
                if from == &id("alight") && to == &id("d")
        ));
    }

    #[test]
    fn expansion_bound_is_reported_instead_of_silently_certifying_partial_top_k() {
        let network = network(
            vec![
                service("first", TransitMode::Bus, &["o", "d"]),
                service("second", TransitMode::Bus, &["o", "d"]),
            ],
            vec![
                trip("first-trip", "first", &[0, 100]),
                trip("second-trip", "second", &[0, 110]),
            ],
        );
        let topology = TransitTopologyIndex::new(&network);
        let transfers = TransferShortcutIndex::from_network(&network);
        let mut query = query("o", "d", &[TransitMode::Bus], 0, 2);
        query.max_static_search_expansions = 1;

        let (_, stats) = generate(&network, &topology, &transfers, &query);

        assert!(stats.static_search_limit_reached);
    }
}
