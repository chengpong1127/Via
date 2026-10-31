use super::*;

pub(super) fn reference(mut candidates: Vec<EvaluatedCandidate>) -> Vec<EvaluatedCandidate> {
    candidates.sort_by_cached_key(|candidate| {
        (
            candidate.metrics.arrival_time,
            candidate.metrics.transfer_count,
            candidate.metrics.walking_seconds,
            candidate.journey.signature(),
        )
    });
    let mut frontier: Vec<EvaluatedCandidate> = Vec::new();
    for candidate in candidates {
        if frontier.iter().any(|known| {
            known.journey.signature() == candidate.journey.signature()
                || can_dominate(known, &candidate)
        }) {
            continue;
        }
        frontier.retain(|known| !can_dominate(&candidate, known));
        frontier.push(candidate);
    }
    frontier
}

fn can_dominate(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> bool {
    !left
        .journey
        .movements()
        .iter()
        .any(|movement| !matches!(movement.kind, JourneyMovementKind::Transit { .. }))
        && left.metrics.dominates(&right.metrics)
}

pub(super) fn assert_equal(left: &[EvaluatedCandidate], right: &[EvaluatedCandidate]) {
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(right) {
        assert_eq!(left.journey.signature(), right.journey.signature());
        assert_eq!(left.journey, right.journey);
        assert_eq!(left.metrics, right.metrics);
    }
}
