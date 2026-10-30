use super::*;

#[derive(Clone)]
pub(super) struct EvaluatedCandidate {
    pub metrics: PathMetrics,
    pub journey: RealizedJourney,
}

struct SignedCandidate {
    candidate: EvaluatedCandidate,
    signature: String,
    needs_hydration: bool,
}

pub(super) fn pareto_candidates(candidates: Vec<EvaluatedCandidate>) -> Vec<EvaluatedCandidate> {
    let mut candidates = candidates
        .into_iter()
        .map(|candidate| SignedCandidate {
            signature: candidate.journey.signature(),
            needs_hydration: candidate.journey.movements().iter().any(needs_hydration),
            candidate,
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
    let mut pareto: Vec<SignedCandidate> = Vec::new();
    for candidate in candidates {
        if pareto
            .iter()
            .any(|known| known.signature == candidate.signature || can_dominate(known, &candidate))
        {
            continue;
        }
        pareto.retain(|known| !can_dominate(&candidate, known));
        pareto.push(candidate);
    }
    pareto.into_iter().map(|entry| entry.candidate).collect()
}

fn can_dominate(left: &SignedCandidate, right: &SignedCandidate) -> bool {
    !left.needs_hydration && left.candidate.metrics.dominates(&right.candidate.metrics)
}

fn needs_hydration(movement: &JourneyMovement) -> bool {
    !matches!(movement.kind, JourneyMovementKind::Transit { .. })
}

pub(super) fn prune_states(states: Vec<EvaluationState>, limit: usize) -> Vec<EvaluationState> {
    let mut states = states
        .into_iter()
        .map(|state| ScoredState {
            metrics: state.metrics(),
            state,
        })
        .collect::<Vec<_>>();
    states.sort_by(|left, right| {
        (
            left.state.ready_at,
            left.metrics.walking_seconds,
            left.metrics.transfer_count,
        )
            .cmp(&(
                right.state.ready_at,
                right.metrics.walking_seconds,
                right.metrics.transfer_count,
            ))
    });
    let mut frontier = Vec::<ScoredState>::new();
    for scored in states {
        if frontier.iter().any(|known| {
            same_capability(&known.state, &scored.state)
                && no_worse(&known.metrics, &scored.metrics)
        }) {
            continue;
        }
        frontier.retain(|known| {
            !(same_capability(&scored.state, &known.state)
                && no_worse(&scored.metrics, &known.metrics))
        });
        frontier.push(scored);
    }
    frontier.truncate(limit.max(1));
    frontier.into_iter().map(|scored| scored.state).collect()
}

struct ScoredState {
    state: EvaluationState,
    metrics: PathMetrics,
}
pub(super) fn no_worse(left: &PathMetrics, right: &PathMetrics) -> bool {
    left.same_timing_quality(right)
        && left.arrival_time <= right.arrival_time
        && left.walking_seconds <= right.walking_seconds
        && left.bicycle_seconds <= right.bicycle_seconds
        && left.bike_share_uses <= right.bike_share_uses
        && left.transfer_count <= right.transfer_count
}
pub(super) fn same_capability(left: &EvaluationState, right: &EvaluationState) -> bool {
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
