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

pub(super) struct EvaluatedCandidate {
    pub metrics: PathMetrics,
    pub variant: JourneyVariant,
}

pub(super) fn pareto_candidates(
    mut candidates: Vec<EvaluatedCandidate>,
    limit: usize,
) -> Vec<EvaluatedCandidate> {
    candidates.sort_by_cached_key(|candidate| {
        (
            candidate.metrics.arrival_time,
            candidate.metrics.transfer_count,
            candidate.metrics.walking_seconds,
            candidate.variant.identity().canonical_signature(),
        )
    });
    let mut pareto = Vec::<EvaluatedCandidate>::new();
    for candidate in candidates {
        if pareto.iter().any(|known| {
            known.variant.identity() == candidate.variant.identity()
                || known.metrics.dominates(&candidate.metrics)
        }) {
            continue;
        }
        pareto.retain(|known| !candidate.metrics.dominates(&known.metrics));
        pareto.push(candidate);
    }
    pareto.truncate(limit.max(1));
    pareto
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
    left.last_was_transit == right.last_was_transit
        && left.bike_reuse_available_at == right.bike_reuse_available_at
        && match (&left.active_bike_run, &right.active_bike_run) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.start_anchor == right.start_anchor
                    && left.started_at == right.started_at
                    && left.shortcut == right.shortcut
                    && left.prefix_movements == right.prefix_movements
            }
            _ => false,
        }
}
