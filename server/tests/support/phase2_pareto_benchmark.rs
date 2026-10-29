use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

static TRANSIT: AtomicBool = AtomicBool::new(false);
static MIXED: AtomicBool = AtomicBool::new(false);
static EXPECTED: std::sync::Mutex<Option<Vec<RealizedJourney>>> = std::sync::Mutex::new(None);

pub(crate) fn take_expected() -> Option<Vec<RealizedJourney>> {
    EXPECTED.lock().unwrap().take()
}

#[derive(Default)]
pub(super) struct Counts {
    identity: usize,
    signature: usize,
    dominance: usize,
    peak: usize,
}

pub(super) fn reference(
    mut candidates: Vec<EvaluatedCandidate>,
) -> (Vec<EvaluatedCandidate>, Counts) {
    let mut counts = Counts::default();
    candidates.sort_by_cached_key(|candidate| {
        counts.identity += 1;
        counts.signature += 1;
        (
            candidate.metrics.arrival_time,
            candidate.metrics.transfer_count,
            candidate.metrics.walking_seconds,
            candidate.journey.identity().canonical_signature(),
        )
    });
    let mut frontier: Vec<EvaluatedCandidate> = Vec::new();
    for candidate in candidates {
        if frontier.iter().any(|known| {
            counts.identity += 2;
            known.journey.identity() == candidate.journey.identity() || {
                counts.dominance += 1;
                can_dominate(known, &candidate)
            }
        }) {
            continue;
        }
        frontier.retain(|known| {
            counts.dominance += 1;
            !can_dominate(&candidate, known)
        });
        frontier.push(candidate);
        counts.peak = counts.peak.max(frontier.len());
    }
    (frontier, counts)
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
        assert_eq!(left.journey.identity(), right.journey.identity());
        assert_eq!(left.journey, right.journey);
        assert_eq!(left.metrics, right.metrics);
    }
}

pub(super) fn capture(candidates: &[EvaluatedCandidate]) {
    if std::env::var_os("VIA_PARETO_BENCH").is_none() {
        return;
    }
    let mixed = candidates.iter().any(|c| c.journey.has_bike_share());
    let flag = if mixed { &MIXED } else { &TRANSIT };
    if flag.swap(true, Ordering::SeqCst) {
        return;
    }
    let (expected, counts) = reference(candidates.to_vec());
    *EXPECTED.lock().unwrap() = Some(
        expected
            .iter()
            .map(|candidate| candidate.journey.clone())
            .collect(),
    );
    let mut samples = Vec::new();
    let mut reference_samples = Vec::new();
    for iteration in 0..16 {
        let reference_pool = candidates.to_vec();
        let reference_start = Instant::now();
        let (reference_result, _) = reference(reference_pool);
        let reference_elapsed = reference_start.elapsed().as_micros();
        assert_equal(&expected, &reference_result);
        let pool = candidates.to_vec();
        let start = Instant::now();
        let actual = pareto_candidates(pool);
        let elapsed = start.elapsed().as_micros();
        assert_equal(&expected, &actual);
        if iteration > 0 {
            samples.push(elapsed);
            reference_samples.push(reference_elapsed);
        }
    }
    println!(
        "PARETO_BENCH mixed={mixed} input={} output={} identity={} signature={} dominance={} peak={} samples_us={samples:?} reference_samples_us={reference_samples:?}",
        candidates.len(),
        expected.len(),
        counts.identity,
        counts.signature,
        counts.dominance,
        counts.peak
    );
}
