use super::*;

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

pub(super) fn prune_states(states: Vec<EvaluationState>, limit: usize) -> Vec<EvaluationState> {
    #[cfg(test)]
    if USE_REFERENCE_STATE_PRUNING.load(std::sync::atomic::Ordering::Relaxed) {
        return prune_states_reference(states, limit);
    }
    #[cfg(test)]
    if CAPTURE_STATE_POOLS.load(std::sync::atomic::Ordering::Relaxed) {
        CAPTURED_STATE_POOLS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((states.clone(), limit));
    }
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

#[cfg(test)]
static USE_REFERENCE_STATE_PRUNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static CAPTURE_STATE_POOLS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static CAPTURED_STATE_POOLS: std::sync::Mutex<Vec<(Vec<EvaluationState>, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
#[derive(Default)]
pub(super) struct StatePruningProfile {
    pub path_metrics_computations: usize,
    pub capability_comparisons: usize,
    pub dominance_comparisons: usize,
    pub peak_frontier_size: usize,
}

#[cfg(test)]
pub(super) fn set_reference_state_pruning(enabled: bool) {
    USE_REFERENCE_STATE_PRUNING.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
pub(super) fn start_state_pool_capture() {
    CAPTURED_STATE_POOLS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
    CAPTURE_STATE_POOLS.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
pub(super) fn take_captured_state_pools() -> Vec<(Vec<EvaluationState>, usize)> {
    CAPTURE_STATE_POOLS.store(false, std::sync::atomic::Ordering::Relaxed);
    std::mem::take(
        &mut *CAPTURED_STATE_POOLS
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
    )
}

#[cfg(test)]
pub(super) fn prune_states_reference(
    mut states: Vec<EvaluationState>,
    limit: usize,
) -> Vec<EvaluationState> {
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

#[cfg(test)]
pub(super) fn profile_prune_states_reference(
    mut states: Vec<EvaluationState>,
    limit: usize,
) -> (Vec<EvaluationState>, StatePruningProfile) {
    let mut profile = StatePruningProfile::default();
    states.sort_by_cached_key(|state| {
        profile.path_metrics_computations += 1;
        let metrics = state.metrics();
        (
            state.ready_at,
            metrics.walking_seconds,
            metrics.transfer_count,
        )
    });
    let mut frontier = Vec::<EvaluationState>::new();
    for state in states {
        profile.path_metrics_computations += 1;
        let metrics = state.metrics();
        if frontier.iter().any(|known| {
            profile.capability_comparisons += 1;
            same_capability(known, &state) && {
                profile.dominance_comparisons += 1;
                profile.path_metrics_computations += 1;
                no_worse(&known.metrics(), &metrics)
            }
        }) {
            continue;
        }
        frontier.retain(|known| {
            profile.capability_comparisons += 1;
            !(same_capability(&state, known) && {
                profile.dominance_comparisons += 1;
                profile.path_metrics_computations += 1;
                no_worse(&metrics, &known.metrics())
            })
        });
        frontier.push(state);
        profile.peak_frontier_size = profile.peak_frontier_size.max(frontier.len());
    }
    frontier.truncate(limit.max(1));
    (frontier, profile)
}
