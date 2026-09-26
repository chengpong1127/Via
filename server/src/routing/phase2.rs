//! Shared state, result ordering, and strategy configuration for Phase 2.

use std::{cmp::Ordering, str::FromStr};

use time::OffsetDateTime;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    realtime::NormalizedRealtimeValue,
    transit::TransitMode,
    transit_runtime::TransitRuntime,
    transit_timing::{EffectiveTrip, TimingBasis, TimingReliability},
};

use super::{
    model::{AccessPoint, EgressPoint, JourneyCandidate, JourneySegment},
    static_search::{CandidateCorridor, StaticCorridor, StaticCorridorStep},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase2Strategy {
    #[default]
    Union,
    Fixed,
    Compare,
}

impl FromStr for Phase2Strategy {
    type Err = Phase2StrategyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "union" => Ok(Self::Union),
            "fixed" => Ok(Self::Fixed),
            "compare" => Ok(Self::Compare),
            _ => Err(Phase2StrategyParseError {
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid Phase 2 strategy {value:?}; expected union, fixed, or compare")]
pub struct Phase2StrategyParseError {
    value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Phase2ComparisonStats {
    pub union_latency_us: u64,
    pub fixed_latency_us: u64,
    pub union_candidate_count: usize,
    pub fixed_candidate_count: usize,
    pub union_departure_lookups: usize,
    pub fixed_departure_lookups: usize,
    pub union_states_expanded: usize,
    pub fixed_states_evaluated: usize,
    pub same_best_exact_signature: bool,
    pub same_best_structural_signature: bool,
    pub union_best_in_retained_topk_exact: bool,
    pub union_best_in_retained_topk_structural: bool,
    pub recombination_used: bool,
    pub best_arrival_delta_seconds: Option<i64>,
    pub fixed_no_route_union_route: bool,
    pub union_no_route_fixed_route: bool,
}

pub(super) struct Phase2Run<'a> {
    pub candidates: &'a [JourneyCandidate],
    pub latency_us: u64,
    pub departure_lookups: usize,
    pub states_evaluated: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactSignature {
    access_stop_id: BoardingPointId,
    steps: Vec<ExactStep>,
    egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExactStep {
    Transit {
        pattern_id: TransitPatternId,
        board_index: usize,
        alight_index: usize,
        board_stop_id: BoardingPointId,
        alight_stop_id: BoardingPointId,
    },
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StructuralSignature {
    access_stop_id: BoardingPointId,
    steps: Vec<StructuralStep>,
    egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StructuralStep {
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct CandidateSignatures {
    exact: ExactSignature,
    structural: StructuralSignature,
}

#[derive(Clone)]
pub(super) struct Phase2Path {
    access: AccessPoint,
    segments: Vec<JourneySegment>,
    walking_seconds: u32,
    evidence: Vec<NormalizedRealtimeValue>,
    timing_bases: Vec<TimingBasis>,
    timing_reliability: TimingReliability,
    uncertainty_seconds: Option<u32>,
}

impl Phase2Path {
    pub(super) fn from_access(access: AccessPoint) -> Self {
        Self {
            walking_seconds: access.walking_seconds,
            access,
            segments: Vec::new(),
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
            uncertainty_seconds: None,
        }
    }

    pub(super) fn with_ride(
        &self,
        trip: &EffectiveTrip,
        board_index: usize,
        alight_index: usize,
    ) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::Transit {
            trip: trip.clone(),
            board_index,
            alight_index,
        });
        next.evidence.extend(trip.evidence.clone());
        next.timing_bases.push(trip.timing_basis);
        if trip.timing_reliability == TimingReliability::Fallback {
            next.timing_reliability = TimingReliability::Fallback;
        }
        next.uncertainty_seconds = match (next.uncertainty_seconds, trip.uncertainty_seconds) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (None, value) | (value, None) => value,
        };
        next
    }

    pub(super) fn with_walking_transfer(
        &self,
        shortcut: &super::transfers::TransferShortcut,
    ) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: shortcut.clone(),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.duration_seconds);
        next
    }

    pub(super) fn walking_seconds(&self) -> u32 {
        self.walking_seconds
    }
}

pub(super) fn make_candidate(
    path: &Phase2Path,
    egress: &EgressPoint,
    arrival_time: OffsetDateTime,
    transit_legs: usize,
) -> JourneyCandidate {
    JourneyCandidate {
        access: path.access.clone(),
        egress: egress.clone(),
        segments: path.segments.clone(),
        departure_time: path.access.departure_at,
        arrival_time,
        walking_seconds: path.walking_seconds.saturating_add(egress.walking_seconds),
        transfer_count: u32::try_from(transit_legs.saturating_sub(1)).unwrap_or(u32::MAX),
        uncertainty_seconds: path.uncertainty_seconds,
        evidence: path.evidence.clone(),
        timing_bases: path.timing_bases.clone(),
        timing_reliability: path.timing_reliability,
    }
}

pub(super) fn pareto_candidates(
    mut candidates: Vec<JourneyCandidate>,
    limit: usize,
) -> Vec<JourneyCandidate> {
    candidates.sort_by(candidate_order);
    let mut pareto = Vec::new();
    for candidate in candidates {
        if pareto.iter().any(|existing: &JourneyCandidate| {
            existing.dominates(&candidate) || existing.signature() == candidate.signature()
        }) {
            continue;
        }
        pareto.retain(|existing| !candidate.dominates(existing));
        pareto.push(candidate);
    }
    pareto.truncate(limit.max(1));
    pareto
}

pub(super) fn candidate_order(left: &JourneyCandidate, right: &JourneyCandidate) -> Ordering {
    left.arrival_time
        .cmp(&right.arrival_time)
        .then(left.transfer_count.cmp(&right.transfer_count))
        .then(left.walking_seconds.cmp(&right.walking_seconds))
        .then(left.signature().cmp(&right.signature()))
}

pub(super) fn candidate_debug_summary(
    network: &TransitRuntime,
    candidate: &JourneyCandidate,
) -> String {
    let segments = candidate
        .segments
        .iter()
        .map(|segment| match segment {
            JourneySegment::Transit {
                trip,
                board_index,
                alight_index,
            } => {
                let mode = network
                    .pattern(&trip.pattern_id)
                    .map(|pattern| network.route_of(pattern).mode);
                let board = trip.stop_times.get(*board_index);
                let alight = trip.stop_times.get(*alight_index);
                format!(
                    "transit(mode={mode:?},pattern={},trip={},board={}@{:?},alight={}@{:?})",
                    trip.pattern_id.as_str(),
                    trip.id.as_str(),
                    board.map(|stop| stop.stop_id.as_str()).unwrap_or("?"),
                    board.map(|stop| stop.departure_at),
                    alight.map(|stop| stop.stop_id.as_str()).unwrap_or("?"),
                    alight.map(|stop| stop.arrival_at),
                )
            }
            JourneySegment::WalkingTransfer { shortcut } => format!(
                "walking_transfer(from={},to={},seconds={})",
                shortcut.from.as_str(),
                shortcut.to.as_str(),
                shortcut.duration_seconds
            ),
        })
        .collect::<Vec<_>>()
        .join(" -> ");
    format!(
        "access={} egress={} depart={:?} arrive={:?} transfers={} walking_seconds={} patterns={:?} path=[{}]",
        candidate.access.stop_id.as_str(),
        candidate.egress.stop_id.as_str(),
        candidate.departure_time,
        candidate.arrival_time,
        candidate.transfer_count,
        candidate.walking_seconds,
        candidate
            .transit_pattern_sequence()
            .iter()
            .map(|pattern| pattern.as_str())
            .collect::<Vec<_>>(),
        segments,
    )
}

pub(super) fn comparison_stats(
    network: &TransitRuntime,
    corridor: &CandidateCorridor,
    union: Phase2Run<'_>,
    fixed: Phase2Run<'_>,
) -> Phase2ComparisonStats {
    let union_best = union
        .candidates
        .iter()
        .min_by(|left, right| candidate_order(left, right));
    let fixed_best = fixed
        .candidates
        .iter()
        .min_by(|left, right| candidate_order(left, right));
    let retained = corridor
        .retained()
        .iter()
        .map(|retained| corridor_signatures(network, retained))
        .collect::<Vec<_>>();
    let union_signature = union_best.and_then(|candidate| candidate_signatures(network, candidate));
    let fixed_signature = fixed_best.and_then(|candidate| candidate_signatures(network, candidate));
    let union_best_in_retained_topk_exact = union_signature
        .as_ref()
        .is_some_and(|signature| retained.iter().any(|(exact, _)| exact == &signature.exact));
    let union_best_in_retained_topk_structural =
        union_signature.as_ref().is_some_and(|signature| {
            retained
                .iter()
                .any(|(_, structural)| structural == &signature.structural)
        });

    Phase2ComparisonStats {
        union_latency_us: union.latency_us,
        fixed_latency_us: fixed.latency_us,
        union_candidate_count: union.candidates.len(),
        fixed_candidate_count: fixed.candidates.len(),
        union_departure_lookups: union.departure_lookups,
        fixed_departure_lookups: fixed.departure_lookups,
        union_states_expanded: union.states_evaluated,
        fixed_states_evaluated: fixed.states_evaluated,
        same_best_exact_signature: matches!(
            (&union_signature, &fixed_signature),
            (Some(union), Some(fixed)) if union.exact == fixed.exact
        ),
        same_best_structural_signature: matches!(
            (&union_signature, &fixed_signature),
            (Some(union), Some(fixed)) if union.structural == fixed.structural
        ),
        union_best_in_retained_topk_exact,
        union_best_in_retained_topk_structural,
        recombination_used: union_best.is_some() && !union_best_in_retained_topk_structural,
        best_arrival_delta_seconds: union_best.zip(fixed_best).map(|(union, fixed)| {
            (union.arrival_time - fixed.arrival_time)
                .whole_seconds()
                .clamp(i64::MIN, i64::MAX)
        }),
        fixed_no_route_union_route: fixed_best.is_none() && union_best.is_some(),
        union_no_route_fixed_route: union_best.is_none() && fixed_best.is_some(),
    }
}

fn candidate_signatures(
    network: &TransitRuntime,
    candidate: &JourneyCandidate,
) -> Option<CandidateSignatures> {
    let mut exact_steps = Vec::with_capacity(candidate.segments.len());
    let mut structural_steps = Vec::with_capacity(candidate.segments.len());
    for segment in &candidate.segments {
        match segment {
            JourneySegment::Transit {
                trip,
                board_index,
                alight_index,
            } => {
                let pattern = network.pattern(&trip.pattern_id)?;
                let mode = network.route_of(pattern).mode;
                let board_stop_id = trip.stop_times.get(*board_index)?.stop_id.clone();
                let alight_stop_id = trip.stop_times.get(*alight_index)?.stop_id.clone();
                exact_steps.push(ExactStep::Transit {
                    pattern_id: trip.pattern_id.clone(),
                    board_index: *board_index,
                    alight_index: *alight_index,
                    board_stop_id: board_stop_id.clone(),
                    alight_stop_id: alight_stop_id.clone(),
                });
                structural_steps.push(StructuralStep::Transit {
                    mode,
                    board_stop_id,
                    alight_stop_id,
                });
            }
            JourneySegment::WalkingTransfer { shortcut } => {
                exact_steps.push(ExactStep::WalkingTransfer {
                    from: shortcut.from.clone(),
                    to: shortcut.to.clone(),
                });
                structural_steps.push(StructuralStep::WalkingTransfer {
                    from: shortcut.from.clone(),
                    to: shortcut.to.clone(),
                });
            }
        }
    }
    Some(CandidateSignatures {
        exact: ExactSignature {
            access_stop_id: candidate.access.stop_id.clone(),
            steps: exact_steps,
            egress_stop_id: candidate.egress.stop_id.clone(),
        },
        structural: StructuralSignature {
            access_stop_id: candidate.access.stop_id.clone(),
            steps: structural_steps,
            egress_stop_id: candidate.egress.stop_id.clone(),
        },
    })
}

fn corridor_signatures(
    network: &TransitRuntime,
    corridor: &StaticCorridor,
) -> (ExactSignature, StructuralSignature) {
    let mut exact_steps = Vec::with_capacity(corridor.steps.len());
    let mut structural_steps = Vec::with_capacity(corridor.steps.len());
    for step in &corridor.steps {
        match step {
            StaticCorridorStep::Transit(leg) => {
                if let Some(pattern) = network.pattern(&leg.pattern_id) {
                    let mode = network.route_of(pattern).mode;
                    exact_steps.push(ExactStep::Transit {
                        pattern_id: leg.pattern_id.clone(),
                        board_index: leg.board_index,
                        alight_index: leg.alight_index,
                        board_stop_id: leg.board_stop_id.clone(),
                        alight_stop_id: leg.alight_stop_id.clone(),
                    });
                    structural_steps.push(StructuralStep::Transit {
                        mode,
                        board_stop_id: leg.board_stop_id.clone(),
                        alight_stop_id: leg.alight_stop_id.clone(),
                    });
                }
            }
            StaticCorridorStep::WalkingTransfer { from, to, .. } => {
                exact_steps.push(ExactStep::WalkingTransfer {
                    from: from.clone(),
                    to: to.clone(),
                });
                structural_steps.push(StructuralStep::WalkingTransfer {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
    }
    (
        ExactSignature {
            access_stop_id: corridor.access_stop_id.clone(),
            steps: exact_steps,
            egress_stop_id: corridor.egress_stop_id.clone(),
        },
        StructuralSignature {
            access_stop_id: corridor.access_stop_id.clone(),
            steps: structural_steps,
            egress_stop_id: corridor.egress_stop_id.clone(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::Phase2Strategy;

    #[test]
    fn phase2_strategy_parses_supported_values() {
        assert_eq!("union".parse(), Ok(Phase2Strategy::Union));
        assert_eq!("fixed".parse(), Ok(Phase2Strategy::Fixed));
        assert_eq!("compare".parse(), Ok(Phase2Strategy::Compare));
        assert!("unsupported".parse::<Phase2Strategy>().is_err());
    }
}
