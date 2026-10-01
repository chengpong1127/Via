use super::*;

pub(super) fn same_timing_bases(left: &[TimingBasis], right: &[TimingBasis]) -> bool {
    fn consecutive_unique(bases: &[TimingBasis]) -> Vec<TimingBasis> {
        let mut normalized = Vec::with_capacity(bases.len());
        for basis in bases {
            if normalized.last() != Some(basis) {
                normalized.push(*basis);
            }
        }
        normalized
    }

    consecutive_unique(left) == consecutive_unique(right)
}

pub(super) fn make_candidate(
    path: &EvaluationPath,
    egress: &EgressPoint,
    arrival_time: OffsetDateTime,
) -> RealizedJourney {
    RealizedJourney {
        access: path.access.clone(),
        egress: egress.clone(),
        segments: path.segments.clone(),
        departure_time: path.access.departure_at,
        arrival_time,
        walking_seconds: path.walking_seconds.saturating_add(egress.walking_seconds),
        transfer_count: physical_transfer_count(&path.segments),
        uncertainty_seconds: path.uncertainty_seconds,
        evidence: path.evidence.clone(),
        timing_bases: path.timing_bases.clone(),
        timing_reliability: path.timing_reliability,
    }
}

pub(super) struct EvaluatedCandidate {
    pub(super) metrics: RealizedJourney,
    pub(super) journey_order_key: String,
    pub(super) variant: JourneyVariant,
    pub(super) variant_identity: JourneyVariantIdentity,
    pub(super) bike_share_uses: usize,
    pub(super) bicycle_seconds: u32,
}

impl EvaluatedCandidate {
    pub(super) fn dominates(&self, other: &Self) -> bool {
        (self.metrics.timing_reliability == other.metrics.timing_reliability
            && same_timing_bases(&self.metrics.timing_bases, &other.metrics.timing_bases)
            && self.metrics.evidence == other.metrics.evidence
            || (other.metrics.timing_reliability == TimingReliability::Reliable
                && other.metrics.timing_bases.is_empty()
                && other.metrics.evidence.is_empty()))
            && self.bike_share_uses <= other.bike_share_uses
            && self.bicycle_seconds <= other.bicycle_seconds
            && self.metrics.arrival_time <= other.metrics.arrival_time
            && self.metrics.walking_seconds <= other.metrics.walking_seconds
            && self.metrics.transfer_count <= other.metrics.transfer_count
            && (self.metrics.arrival_time < other.metrics.arrival_time
                || self.metrics.walking_seconds < other.metrics.walking_seconds
                || self.metrics.transfer_count < other.metrics.transfer_count
                || self.bike_share_uses < other.bike_share_uses
                || self.bicycle_seconds < other.bicycle_seconds)
    }
}

pub(super) fn make_variant(
    candidate: RealizedJourney,
    bike_shortcuts: &[(usize, BikeShortcut)],
) -> Option<JourneyVariant> {
    let skeleton = JourneySkeleton::from_candidate(candidate).ok()?;
    let mut variant = JourneyVariant::baseline(skeleton);
    for (segment_index, shortcut) in bike_shortcuts {
        let movement_index = segment_index.saturating_add(1);
        let movement = variant.skeleton().movements().get(movement_index)?;
        let mut shortcut = shortcut.clone();
        shortcut.from_anchor = movement.from_anchor;
        shortcut.to_anchor = movement.to_anchor;
        variant = JourneyVariant::with_shortcut(&variant, shortcut)?;
    }
    Some(variant)
}

pub(super) fn pareto_candidates(
    mut candidates: Vec<EvaluatedCandidate>,
    limit: usize,
) -> Vec<EvaluatedCandidate> {
    candidates.sort_by(|left, right| {
        candidate_order(
            &left.metrics,
            &left.journey_order_key,
            &right.metrics,
            &right.journey_order_key,
        )
    });
    let mut pareto = Vec::new();
    for candidate in candidates {
        if pareto.iter().any(|existing: &EvaluatedCandidate| {
            existing.variant_identity == candidate.variant_identity
                || existing.dominates(&candidate)
        }) {
            continue;
        }
        pareto.retain(|existing: &EvaluatedCandidate| !candidate.dominates(existing));
        pareto.push(candidate);
    }
    pareto.truncate(limit.max(1));
    pareto
}

fn candidate_order(
    left: &RealizedJourney,
    left_order_key: &str,
    right: &RealizedJourney,
    right_order_key: &str,
) -> Ordering {
    left.arrival_time
        .cmp(&right.arrival_time)
        .then(left.transfer_count.cmp(&right.transfer_count))
        .then(left.walking_seconds.cmp(&right.walking_seconds))
        .then(left_order_key.cmp(right_order_key))
}

pub(super) fn prune_states(mut states: Vec<EvaluationState>, limit: usize) -> Vec<EvaluationState> {
    states.sort_by_cached_key(|state| {
        (
            state.ready_at,
            state.path.walking_seconds(),
            state.transit_legs,
            state.path.deterministic_order_key(),
        )
    });
    let mut frontier = Vec::<EvaluationState>::new();
    for state in states {
        if frontier.iter().any(|known| {
            known.path.bike_shortcuts.len() <= state.path.bike_shortcuts.len()
                && known.last_was_transit == state.last_was_transit
                && same_bike_capability(known, &state)
                && known.path.same_timing_quality(&state.path)
                && known.ready_at <= state.ready_at
                && known.path.walking_seconds() <= state.path.walking_seconds()
                && known.path.bicycle_seconds() <= state.path.bicycle_seconds()
                && known.transit_legs <= state.transit_legs
        }) {
            continue;
        }
        frontier.retain(|known| {
            !(state.path.bike_shortcuts.len() <= known.path.bike_shortcuts.len()
                && state.last_was_transit == known.last_was_transit
                && same_bike_capability(&state, known)
                && state.path.same_timing_quality(&known.path)
                && state.ready_at <= known.ready_at
                && state.path.walking_seconds() <= known.path.walking_seconds()
                && state.path.bicycle_seconds() <= known.path.bicycle_seconds()
                && state.transit_legs <= known.transit_legs)
        });
        frontier.push(state);
    }
    frontier.truncate(limit.max(1));
    frontier
}

fn same_bike_capability(left: &EvaluationState, right: &EvaluationState) -> bool {
    if left.bike_reuse_available_at != right.bike_reuse_available_at {
        return false;
    }
    match (&left.active_bike_run, &right.active_bike_run) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.start_anchor == right.start_anchor
                && left.start_stop_id == right.start_stop_id
                && left.started_at == right.started_at
                && left.shortcut == right.shortcut
                && left.prefix_path.identity() == right.prefix_path.identity()
                && left.prefix_path.same_timing_quality(&right.prefix_path)
        }
        _ => false,
    }
}
