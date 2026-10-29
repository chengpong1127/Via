use super::*;

pub(super) fn same_timing_bases(left: &[TimingBasis], right: &[TimingBasis]) -> bool {
    fn unique(bases: &[TimingBasis]) -> Vec<TimingBasis> {
        let mut normalized = Vec::new();
        for basis in bases {
            if normalized.last() != Some(basis) {
                normalized.push(*basis);
            }
        }
        normalized
    }
    unique(left) == unique(right)
}

#[cfg_attr(test, derive(Clone))]
pub(super) struct EvaluatedCandidate {
    pub metrics: PathMetrics,
    pub journey: RealizedJourney,
}

struct CandidateMetadata<I> {
    candidate: EvaluatedCandidate,
    identity: I,
    signature: String,
    needs_hydration: bool,
}

pub(super) fn pareto_candidates(candidates: Vec<EvaluatedCandidate>) -> Vec<EvaluatedCandidate> {
    let mut candidates = candidates
        .into_iter()
        .map(|candidate| {
            let identity = candidate.journey.identity();
            let signature = identity.canonical_signature();
            let needs_hydration = candidate.journey.movements().iter().any(needs_hydration);
            CandidateMetadata {
                candidate,
                identity,
                signature,
                needs_hydration,
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_metrics = &left.candidate.metrics;
        let right_metrics = &right.candidate.metrics;
        (
            left_metrics.arrival_time,
            left_metrics.transfer_count,
            left_metrics.walking_seconds,
            &left.signature,
        )
            .cmp(&(
                right_metrics.arrival_time,
                right_metrics.transfer_count,
                right_metrics.walking_seconds,
                &right.signature,
            ))
    });
    let mut pareto: Vec<CandidateMetadata<_>> = Vec::new();
    for candidate in candidates {
        if pareto
            .iter()
            .any(|known| known.identity == candidate.identity || can_dominate(known, &candidate))
        {
            continue;
        }
        pareto.retain(|known| !can_dominate(&candidate, known));
        pareto.push(candidate);
    }
    pareto.into_iter().map(|entry| entry.candidate).collect()
}

fn can_dominate<I>(left: &CandidateMetadata<I>, right: &CandidateMetadata<I>) -> bool {
    !left.needs_hydration && left.candidate.metrics.dominates(&right.candidate.metrics)
}

fn needs_hydration(movement: &JourneyMovement) -> bool {
    !matches!(movement.kind, JourneyMovementKind::Transit { .. })
}

pub(super) fn prune_states(mut states: Vec<EvaluationState>, limit: usize) -> Vec<EvaluationState> {
    states.sort_by_cached_key(|state| {
        let metrics = state.metrics();
        (
            state.ready_at,
            metrics.walking_seconds,
            metrics.transfer_count,
        )
    });
    let mut frontier = Vec::<EvaluationState>::new();
    for state in states {
        let metrics = state.metrics();
        if frontier
            .iter()
            .any(|known| same_capability(known, &state) && no_worse(&known.metrics(), &metrics))
        {
            continue;
        }
        frontier.retain(|known| {
            !(same_capability(&state, known) && no_worse(&metrics, &known.metrics()))
        });
        frontier.push(state);
    }
    frontier.truncate(limit.max(1));
    frontier
}
fn no_worse(left: &PathMetrics, right: &PathMetrics) -> bool {
    left.same_timing_quality(right)
        && left.arrival_time <= right.arrival_time
        && left.walking_seconds <= right.walking_seconds
        && left.bicycle_seconds <= right.bicycle_seconds
        && left.bike_share_uses <= right.bike_share_uses
        && left.transfer_count <= right.transfer_count
}
fn same_capability(left: &EvaluationState, right: &EvaluationState) -> bool {
    (!(left.movements.iter().any(needs_hydration) || right.movements.iter().any(needs_hydration))
        || left.movements == right.movements)
        && left.last_was_transit == right.last_was_transit
        && left.bike_reuse_available_at == right.bike_reuse_available_at
        && match (&left.active_bike_rental, &right.active_bike_rental) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.start_anchor == right.start_anchor
                    && left.started_at == right.started_at
                    && left.movement == right.movement
                    && left.prefix_movements == right.prefix_movements
            }
            _ => false,
        }
}
