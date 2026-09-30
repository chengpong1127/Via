use std::{cmp::Ordering, collections::HashMap, ops::ControlFlow, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::domain::{
    bikeshare::BikeAvailabilitySnapshot,
    ids::{BikeStationId, BoardingPointId, EffectiveTripId},
    realtime::NormalizedRealtimeValue,
    transit_runtime::TransitRuntime,
    transit_timing::{EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot},
};

use super::{
    bike::BikeSegmentPlanner,
    model::{
        AccessPoint, EgressPoint, JourneySegment, RealizedJourney, RoutingError, TransitQuery,
        physical_transfer_count,
    },
    static_search::{RetainedCorridorSet, StaticCorridor, StaticCorridorStep, StaticTransitLeg},
    transfers::TransferShortcut,
    variant::{BikeShortcut, JourneySkeleton, JourneyVariant, JourneyVariantIdentity},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phase2EvaluationStats {
    pub corridors_evaluated: usize,
    pub corridors_feasible: usize,
    pub corridor_states_evaluated: usize,
    pub transit_departure_lookups: usize,
    pub effective_trips_examined: usize,
    pub surface_transfers_evaluated: usize,
    pub walking_realizations_considered: usize,
    pub bike_realizations_considered: usize,
    pub bike_realizations_feasible: usize,
    pub bike_realizations_selected: usize,
    pub bike_lower_bound_checks: usize,
    pub bike_lower_bound_rejections: usize,
    pub bike_exact_candidates_requested: usize,
    pub bike_exact_cache_hits: usize,
    pub bike_matrix_requests: usize,
    pub bike_continuous_extensions: usize,
    pub infeasible_no_departure: usize,
    pub infeasible_transfer: usize,
    pub infeasible_access: usize,
    pub infeasible_constraints: usize,
    pub malformed_trips: usize,
    pub candidates_before_pareto: usize,
    pub final_candidates: usize,
    pub phase2_latency_us: u64,
    pub expansion_limit_reached: bool,
}

#[derive(Clone)]
struct EvaluationState {
    stop_id: BoardingPointId,
    ready_at: OffsetDateTime,
    transit_legs: usize,
    last_was_transit: bool,
    active_bike_run: Option<ActiveBikeRun>,
    bike_reuse_available_at: Option<OffsetDateTime>,
    path: EvaluationPath,
}

#[derive(Clone)]
struct ActiveBikeRun {
    start_anchor: usize,
    start_stop_id: BoardingPointId,
    started_at: OffsetDateTime,
    prefix_path: EvaluationPath,
    shortcut: BikeShortcut,
}

#[cfg(test)]
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CorridorMovement {
    from_anchor: usize,
    to_anchor: usize,
    from: BoardingPointId,
    to: BoardingPointId,
}

#[cfg(test)]
type TestBikeRoutes = HashMap<CorridorMovement, Vec<BikeShortcut>>;

impl EvaluationState {
    fn after_normal_transition(
        &self,
        stop_id: BoardingPointId,
        ready_at: OffsetDateTime,
        transit_legs: usize,
        last_was_transit: bool,
        path: EvaluationPath,
    ) -> Self {
        let mut bike_reuse_available_at = self.bike_reuse_available_at;
        if let Some(run) = &self.active_bike_run {
            let return_seconds = run
                .shortcut
                .walk_to_pickup
                .duration_seconds
                .saturating_add(run.shortcut.bicycle_duration_seconds);
            let reuse_at = run.started_at
                + Duration::seconds(i64::from(return_seconds))
                + Duration::minutes(15);
            bike_reuse_available_at =
                Some(bike_reuse_available_at.map_or(reuse_at, |known| known.max(reuse_at)));
        }
        Self {
            stop_id,
            ready_at,
            transit_legs,
            last_was_transit,
            active_bike_run: None,
            bike_reuse_available_at,
            path,
        }
    }
}

fn bike_start(
    state: &EvaluationState,
    current_anchor: usize,
) -> Option<(usize, BoardingPointId, OffsetDateTime, EvaluationPath)> {
    if let Some(run) = &state.active_bike_run {
        return Some((
            run.start_anchor,
            run.start_stop_id.clone(),
            run.started_at,
            run.prefix_path.clone(),
        ));
    }
    let started_at = state
        .bike_reuse_available_at
        .map_or(state.ready_at, |available| available.max(state.ready_at));
    Some((
        current_anchor,
        state.stop_id.clone(),
        started_at,
        state.path.clone(),
    ))
}

#[derive(Clone)]
struct EvaluationPath {
    access: AccessPoint,
    segments: Vec<JourneySegment>,
    walking_seconds: u32,
    evidence: Vec<NormalizedRealtimeValue>,
    timing_bases: Vec<TimingBasis>,
    timing_reliability: TimingReliability,
    uncertainty_seconds: Option<u32>,
    bike_shortcuts: Vec<(usize, BikeShortcut)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EvaluationPathIdentity {
    segments: Vec<EvaluationSegmentIdentity>,
    bike_shortcuts: Vec<EvaluationBikeIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum EvaluationSegmentIdentity {
    Transit {
        trip_id: EffectiveTripId,
        board_index: usize,
        alight_index: usize,
    },
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
        duration_seconds: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EvaluationBikeIdentity {
    segment_index: usize,
    pickup_station_id: BikeStationId,
    dropoff_station_id: BikeStationId,
}

impl EvaluationPath {
    fn from_access(access: AccessPoint) -> Self {
        Self {
            walking_seconds: access.walking_seconds,
            access,
            segments: Vec::new(),
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
            uncertainty_seconds: None,
            bike_shortcuts: Vec::new(),
        }
    }

    fn with_ride(&self, trip: &EffectiveTrip, board_index: usize, alight_index: usize) -> Self {
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

    fn with_walking_transfer(&self, shortcut: &TransferShortcut) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: shortcut.clone(),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.duration_seconds);
        next
    }

    fn with_bike_transfer(
        &self,
        from: &BoardingPointId,
        to: &BoardingPointId,
        shortcut: &BikeShortcut,
    ) -> Self {
        let mut next = self.clone();
        let segment_index = next.segments.len();
        // This skeleton placeholder is replaced by the exact BikeShare movement.
        // Its total duration includes cycling, so it has no walking distance.
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: TransferShortcut::new(from.clone(), to.clone(), shortcut.total_seconds, 0),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.walk_to_pickup.duration_seconds)
            .saturating_add(shortcut.walk_from_dropoff.duration_seconds);
        next.bike_shortcuts.push((segment_index, shortcut.clone()));
        next
    }

    fn identity(&self) -> EvaluationPathIdentity {
        let segments = self
            .segments
            .iter()
            .map(|segment| match segment {
                JourneySegment::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => EvaluationSegmentIdentity::Transit {
                    trip_id: trip.id.clone(),
                    board_index: *board_index,
                    alight_index: *alight_index,
                },
                JourneySegment::WalkingTransfer { shortcut } => {
                    EvaluationSegmentIdentity::WalkingTransfer {
                        from: shortcut.from.clone(),
                        to: shortcut.to.clone(),
                        duration_seconds: shortcut.duration_seconds,
                    }
                }
            })
            .collect();
        let bike_shortcuts = self
            .bike_shortcuts
            .iter()
            .map(|(index, shortcut)| EvaluationBikeIdentity {
                segment_index: *index,
                pickup_station_id: shortcut.pickup_station_id.clone(),
                dropoff_station_id: shortcut.dropoff_station_id.clone(),
            })
            .collect();
        EvaluationPathIdentity {
            segments,
            bike_shortcuts,
        }
    }

    // Keep the previous lexical tie order while typed identity handles equality.
    fn deterministic_order_key(&self) -> String {
        let segments = self
            .segments
            .iter()
            .map(|segment| match segment {
                JourneySegment::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => format!("transit:{}:{board_index}:{alight_index}", trip.id.as_str()),
                JourneySegment::WalkingTransfer { shortcut } => format!(
                    "transfer:{}:{}:{}",
                    shortcut.from.as_str(),
                    shortcut.to.as_str(),
                    shortcut.duration_seconds
                ),
            })
            .collect::<Vec<_>>()
            .join("|");
        let bikes = self
            .bike_shortcuts
            .iter()
            .map(|(index, shortcut)| {
                format!(
                    "bike:{index}:{}:{}",
                    shortcut.pickup_station_id.as_str(),
                    shortcut.dropoff_station_id.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        format!("{segments}|{bikes}")
    }

    fn walking_seconds(&self) -> u32 {
        self.walking_seconds
    }

    fn bicycle_seconds(&self) -> u32 {
        self.bike_shortcuts
            .iter()
            .map(|(_, shortcut)| shortcut.bicycle_duration_seconds)
            .fold(0, u32::saturating_add)
    }

    fn same_timing_quality(&self, other: &Self) -> bool {
        self.timing_reliability == other.timing_reliability
            && same_timing_bases(&self.timing_bases, &other.timing_bases)
            && self.evidence == other.evidence
    }
}

fn same_timing_bases(left: &[TimingBasis], right: &[TimingBasis]) -> bool {
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

fn make_candidate(
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

struct EvaluatedCandidate {
    metrics: RealizedJourney,
    journey_order_key: String,
    variant: JourneyVariant,
    variant_identity: JourneyVariantIdentity,
    bike_share_uses: usize,
    bicycle_seconds: u32,
}

impl EvaluatedCandidate {
    fn dominates(&self, other: &Self) -> bool {
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

fn make_variant(
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

fn pareto_candidates(
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

#[cfg(test)]
fn evaluate(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<Vec<JourneyVariant>, RoutingError> {
    evaluate_with_stats(network, corridor, snapshot, query).map(|(items, _)| items)
}

#[cfg(test)]
fn evaluate_with_stats(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    run_test_evaluation(network, corridor, snapshot, query, None)
}

#[cfg(test)]
fn evaluate_with_test_bike_routes(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
    bike_realizations: &TestBikeRoutes,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    run_test_evaluation(network, corridor, snapshot, query, Some(bike_realizations))
}

#[cfg(test)]
fn run_test_evaluation(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
    bike_realizations: Option<&TestBikeRoutes>,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(evaluate_inner(
            network,
            corridor,
            snapshot,
            query,
            None,
            bike_realizations,
        ))
}

pub(super) async fn evaluate_with_bike(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
    bike: Option<(
        &BikeSegmentPlanner,
        &BikeAvailabilitySnapshot,
        OffsetDateTime,
    )>,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    #[cfg(test)]
    {
        evaluate_inner(network, corridor, snapshot, query, bike, None).await
    }
    #[cfg(not(test))]
    {
        evaluate_inner(network, corridor, snapshot, query, bike).await
    }
}

async fn evaluate_inner(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
    bike_context: Option<(
        &BikeSegmentPlanner,
        &BikeAvailabilitySnapshot,
        OffsetDateTime,
    )>,
    #[cfg(test)] bike_realizations: Option<&TestBikeRoutes>,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    let started_at = Instant::now();
    if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
        return Err(RoutingError::DeadlineExceeded);
    }
    if query.origin_access.is_empty() || query.destination_egress.is_empty() {
        return Err(RoutingError::InvalidQuery);
    }

    let mut evaluator = FixedCorridorEvaluator {
        network,
        corridor,
        snapshot,
        query: &query,
        bike_context,
        #[cfg(test)]
        bike_realizations,
        egress_by_stop: index_egress(&query.destination_egress),
        stats: Phase2EvaluationStats::default(),
        bike_route_cache: HashMap::new(),
    };
    let mut candidates = Vec::new();
    for retained in corridor.retained() {
        match evaluator.evaluate_corridor(retained).await? {
            ControlFlow::Continue(completed) => candidates.extend(completed),
            ControlFlow::Break(()) => break,
        }
    }
    evaluator.finish(candidates, started_at)
}

struct FixedCorridorEvaluator<'a> {
    network: &'a TransitRuntime,
    corridor: &'a RetainedCorridorSet,
    snapshot: &'a TimingSnapshot,
    query: &'a TransitQuery,
    bike_context: Option<(
        &'a BikeSegmentPlanner,
        &'a BikeAvailabilitySnapshot,
        OffsetDateTime,
    )>,
    #[cfg(test)]
    bike_realizations: Option<&'a TestBikeRoutes>,
    egress_by_stop: HashMap<BoardingPointId, Vec<EgressPoint>>,
    stats: Phase2EvaluationStats,
    bike_route_cache: HashMap<(BoardingPointId, BoardingPointId), Vec<BikeShortcut>>,
}

impl FixedCorridorEvaluator<'_> {
    async fn evaluate_corridor(
        &mut self,
        retained: &StaticCorridor,
    ) -> Result<ControlFlow<(), Vec<EvaluatedCandidate>>, RoutingError> {
        if self
            .query
            .deadline
            .is_some_and(|deadline| deadline.is_elapsed())
        {
            return Err(RoutingError::DeadlineExceeded);
        }
        self.stats.corridors_evaluated += 1;
        let corridor_legs = retained.transit_legs().count();
        if corridor_legs > self.query.limits.max_transfers.saturating_add(1) {
            self.stats.infeasible_transfer += 1;
            return Ok(ControlFlow::Continue(Vec::new()));
        }
        let Some(anchors) = retained.anchors() else {
            self.stats.infeasible_transfer += 1;
            return Ok(ControlFlow::Continue(Vec::new()));
        };
        let anchor_stops = anchors.stops();
        let mut frontiers = vec![Vec::<EvaluationState>::new(); retained.steps.len() + 1];
        frontiers[0] = self
            .query
            .origin_access
            .iter()
            .filter(|access| access.stop_id == retained.access_stop_id)
            .cloned()
            .map(|access| EvaluationState {
                stop_id: access.stop_id.clone(),
                ready_at: access.available_at,
                transit_legs: 0,
                last_was_transit: false,
                active_bike_run: None,
                bike_reuse_available_at: None,
                path: EvaluationPath::from_access(access),
            })
            .collect::<Vec<_>>();
        if frontiers[0].is_empty() {
            self.stats.infeasible_access += 1;
            return Ok(ControlFlow::Continue(Vec::new()));
        }

        let mut saw_missing_departure = false;
        let mut transfer_mismatch = false;
        for from_anchor in 0..retained.steps.len() {
            if self
                .query
                .deadline
                .is_some_and(|deadline| deadline.is_elapsed())
            {
                return Err(RoutingError::DeadlineExceeded);
            }
            let states = if from_anchor == 0 {
                std::mem::take(&mut frontiers[from_anchor])
            } else {
                prune_states(
                    std::mem::take(&mut frontiers[from_anchor]),
                    self.query.limits.max_candidates,
                )
            };
            if states.is_empty() {
                continue;
            }
            let step = &retained.steps[from_anchor];
            for state in states {
                if self
                    .query
                    .deadline
                    .is_some_and(|deadline| deadline.is_elapsed())
                {
                    return Err(RoutingError::DeadlineExceeded);
                }
                if state.stop_id != anchor_stops[from_anchor] {
                    transfer_mismatch = true;
                    continue;
                }
                let ControlFlow::Continue(normal_successors) =
                    self.expand_normal(step, &state, &mut saw_missing_departure)?
                else {
                    return Ok(ControlFlow::Break(()));
                };

                let best_normal_arrival = normal_successors
                    .iter()
                    .map(|successor| successor.ready_at)
                    .min();
                frontiers[from_anchor + 1].extend(normal_successors);

                if self
                    .expand_bike(
                        &state,
                        from_anchor,
                        anchor_stops,
                        best_normal_arrival,
                        &mut frontiers[from_anchor + 1],
                    )
                    .await?
                    .is_break()
                {
                    return Ok(ControlFlow::Break(()));
                }
            }
        }
        let states = prune_states(
            std::mem::take(&mut frontiers[retained.steps.len()]),
            self.query.limits.max_candidates,
        );

        let corridor_candidates =
            self.finalize_corridor(states, retained, &mut transfer_mismatch)?;
        if corridor_candidates.is_empty() {
            if transfer_mismatch {
                self.stats.infeasible_transfer += 1;
            } else if saw_missing_departure {
                self.stats.infeasible_no_departure += 1;
            } else {
                self.stats.infeasible_constraints += 1;
            }
        } else {
            self.stats.corridors_feasible += 1;
            return Ok(ControlFlow::Continue(corridor_candidates));
        }

        Ok(ControlFlow::Continue(Vec::new()))
    }

    fn expand_normal(
        &mut self,
        step: &StaticCorridorStep,
        state: &EvaluationState,
        saw_missing_departure: &mut bool,
    ) -> Result<ControlFlow<(), Vec<EvaluationState>>, RoutingError> {
        match step {
            StaticCorridorStep::SurfaceTransfer {
                from,
                to,
                walking_duration_seconds,
                walking_distance_metres,
            } => self.expand_walking(
                state,
                from,
                to,
                *walking_duration_seconds,
                *walking_distance_metres,
            ),
            StaticCorridorStep::Transit(leg) => {
                self.expand_transit(state, leg, saw_missing_departure)
            }
        }
    }

    fn expand_walking(
        &mut self,
        state: &EvaluationState,
        from: &BoardingPointId,
        to: &BoardingPointId,
        walking_duration_seconds: u32,
        walking_distance_metres: u32,
    ) -> Result<ControlFlow<(), Vec<EvaluationState>>, RoutingError> {
        let mut normal_successors = Vec::new();

        self.stats.surface_transfers_evaluated += 1;
        if charge_state(&mut self.stats, self.query.limits.max_phase2_expansions) {
            self.stats.expansion_limit_reached = true;
            return Ok(ControlFlow::Break(()));
        }
        let ready_at = state.ready_at + Duration::seconds(i64::from(walking_duration_seconds));
        if within_query_bounds(self.query, ready_at) {
            let shortcut = TransferShortcut::new(
                from.clone(),
                to.clone(),
                walking_duration_seconds,
                walking_distance_metres,
            );
            self.stats.walking_realizations_considered += 1;
            normal_successors.push(state.after_normal_transition(
                to.clone(),
                ready_at,
                state.transit_legs,
                false,
                state.path.with_walking_transfer(&shortcut),
            ));
        }

        Ok(ControlFlow::Continue(normal_successors))
    }

    fn expand_transit(
        &mut self,
        state: &EvaluationState,
        leg: &StaticTransitLeg,
        saw_missing_departure: &mut bool,
    ) -> Result<ControlFlow<(), Vec<EvaluationState>>, RoutingError> {
        let mut normal_successors = Vec::new();

        let alternatives = self.corridor.equivalent_legs_for(self.network, leg);
        for alternative in &alternatives {
            if self
                .query
                .deadline
                .is_some_and(|deadline| deadline.is_elapsed())
            {
                return Err(RoutingError::DeadlineExceeded);
            }
            if charge_state(&mut self.stats, self.query.limits.max_phase2_expansions) {
                self.stats.expansion_limit_reached = true;
                return Ok(ControlFlow::Break(()));
            }
            if !valid_pattern_leg(self.network, self.corridor, leg, alternative) {
                continue;
            }
            let boarding_ready_at = if state.last_was_transit {
                state.ready_at
                    + Duration::seconds(i64::from(self.network.same_stop_transfer_seconds()))
            } else {
                state.ready_at
            };
            if !within_query_bounds(self.query, boarding_ready_at) {
                continue;
            }
            self.stats.transit_departure_lookups += 1;
            let departures = self.snapshot.departures(
                &alternative.pattern_id,
                alternative.board_index,
                boarding_ready_at,
                self.query.limits.max_departures_per_query,
            );
            if departures.is_empty() {
                *saw_missing_departure = true;
            }
            self.stats.effective_trips_examined += departures.len();
            for trip in departures {
                if self
                    .query
                    .deadline
                    .is_some_and(|deadline| deadline.is_elapsed())
                {
                    return Err(RoutingError::DeadlineExceeded);
                }
                let Some(arrival_at) =
                    retained_alight_time(self.network, trip, alternative, boarding_ready_at)
                else {
                    self.stats.malformed_trips += 1;
                    continue;
                };
                if !within_query_bounds(self.query, arrival_at)
                    || self
                        .query
                        .latest_arrival
                        .is_some_and(|latest| arrival_at > latest)
                {
                    continue;
                }
                normal_successors.push(
                    state.after_normal_transition(
                        alternative.alight_stop_id.clone(),
                        arrival_at,
                        state.transit_legs + 1,
                        true,
                        state.path.with_ride(
                            trip,
                            alternative.board_index,
                            alternative.alight_index,
                        ),
                    ),
                );
            }
        }

        Ok(ControlFlow::Continue(normal_successors))
    }

    async fn expand_bike(
        &mut self,
        state: &EvaluationState,
        from_anchor: usize,
        anchor_stops: &[BoardingPointId],
        best_normal_arrival: Option<OffsetDateTime>,
        successors: &mut Vec<EvaluationState>,
    ) -> Result<ControlFlow<()>, RoutingError> {
        #[cfg(test)]
        let test_movement = self.bike_realizations.and_then(|realizations| {
            let start_anchor = state
                .active_bike_run
                .as_ref()
                .map_or(from_anchor, |run| run.start_anchor);
            let from = state.active_bike_run.as_ref().map_or_else(
                || anchor_stops[from_anchor].clone(),
                |run| run.start_stop_id.clone(),
            );
            let movement = CorridorMovement {
                from_anchor: start_anchor,
                to_anchor: from_anchor + 1,
                from,
                to: anchor_stops[from_anchor + 1].clone(),
            };
            realizations
                .get(&movement)
                .map(|shortcuts| (movement, shortcuts))
        });

        if let Some((bike_start_anchor, bike_start_stop, bike_started_at, prefix_path)) =
            bike_start(state, from_anchor)
        {
            #[cfg(test)]
            let test_lower_bound = test_movement.as_ref().and_then(|(_, shortcuts)| {
                shortcuts
                    .iter()
                    .map(|shortcut| shortcut.total_seconds / 2)
                    .min()
            });
            #[cfg(test)]
            let candidate = self.bike_context.and_then(|(planner, availability, now)| {
                planner.movement_candidate(
                    &bike_start_stop,
                    &anchor_stops[from_anchor + 1],
                    availability,
                    now,
                )
            });
            #[cfg(not(test))]
            let candidate = self.bike_context.and_then(|(planner, availability, now)| {
                planner.movement_candidate(
                    &bike_start_stop,
                    &anchor_stops[from_anchor + 1],
                    availability,
                    now,
                )
            });
            #[cfg(test)]
            let lower_bound_seconds = candidate
                .as_ref()
                .map(|candidate| candidate.optimistic_lower_bound_seconds)
                .or(test_lower_bound);
            #[cfg(not(test))]
            let lower_bound_seconds = candidate
                .as_ref()
                .map(|candidate| candidate.optimistic_lower_bound_seconds);

            if let Some(lower_bound_seconds) = lower_bound_seconds {
                self.stats.bike_lower_bound_checks += 1;
                let optimistic_arrival =
                    bike_started_at + Duration::seconds(i64::from(lower_bound_seconds));
                if best_normal_arrival.is_some_and(|normal| optimistic_arrival >= normal) {
                    self.stats.bike_lower_bound_rejections += 1;
                } else if !charge_state(&mut self.stats, self.query.limits.max_phase2_expansions) {
                    let route_key = (
                        bike_start_stop.clone(),
                        anchor_stops[from_anchor + 1].clone(),
                    );
                    let mut bike_options =
                        if let Some(cached) = self.bike_route_cache.get(&route_key) {
                            self.stats.bike_exact_cache_hits += 1;
                            cached.clone()
                        } else {
                            self.stats.bike_exact_candidates_requested += 1;
                            #[cfg(test)]
                            let mut exact = if let (Some((planner, _, _)), Some(candidate)) =
                                (self.bike_context, candidate.as_ref())
                            {
                                let (items, bike_stats) =
                                    planner.realize_candidate(candidate, 0, 1).await;
                                if self
                                    .query
                                    .deadline
                                    .is_some_and(|deadline| deadline.is_elapsed())
                                {
                                    return Err(RoutingError::DeadlineExceeded);
                                }
                                self.stats.bike_matrix_requests += bike_stats.matrix_requests;
                                items
                            } else {
                                test_movement
                                    .as_ref()
                                    .map(|(_, shortcuts)| shortcuts.to_vec())
                                    .unwrap_or_default()
                            };
                            #[cfg(not(test))]
                            let mut exact = if let (Some((planner, _, _)), Some(candidate)) =
                                (self.bike_context, candidate.as_ref())
                            {
                                let (items, bike_stats) =
                                    planner.realize_candidate(candidate, 0, 1).await;
                                if self
                                    .query
                                    .deadline
                                    .is_some_and(|deadline| deadline.is_elapsed())
                                {
                                    return Err(RoutingError::DeadlineExceeded);
                                }
                                self.stats.bike_matrix_requests += bike_stats.matrix_requests;
                                items
                            } else {
                                Vec::new()
                            };
                            for shortcut in &mut exact {
                                shortcut.from_anchor = 0;
                                shortcut.to_anchor = 1;
                            }
                            self.bike_route_cache
                                .insert(route_key.clone(), exact.clone());
                            exact
                        };
                    for bike in &mut bike_options {
                        bike.from_anchor = bike_start_anchor;
                        bike.to_anchor = from_anchor + 1;
                    }
                    for bike in bike_options {
                        self.stats.bike_realizations_considered += 1;
                        let route_seconds = bike.total_seconds;
                        let rental_wait_seconds = state.active_bike_run.as_ref().map_or_else(
                            || {
                                u32::try_from((bike_started_at - state.ready_at).whole_seconds())
                                    .unwrap_or(u32::MAX)
                            },
                            |run| run.shortcut.rental_wait_seconds,
                        );
                        let Some(bike) = bike.with_rental_wait(rental_wait_seconds) else {
                            continue;
                        };
                        let ready_at =
                            bike_started_at + Duration::seconds(i64::from(route_seconds));
                        if best_normal_arrival.is_some_and(|normal| ready_at >= normal)
                            || !within_query_bounds(self.query, ready_at)
                        {
                            continue;
                        }
                        self.stats.bike_realizations_feasible += 1;
                        let (path, active_bike_run) = if let Some(run) = &state.active_bike_run {
                            self.stats.bike_continuous_extensions += 1;
                            (
                                run.prefix_path.with_bike_transfer(
                                    &anchor_stops[bike_start_anchor],
                                    &anchor_stops[from_anchor + 1],
                                    &bike,
                                ),
                                ActiveBikeRun {
                                    start_anchor: run.start_anchor,
                                    start_stop_id: run.start_stop_id.clone(),
                                    started_at: run.started_at,
                                    prefix_path: run.prefix_path.clone(),
                                    shortcut: bike.clone(),
                                },
                            )
                        } else {
                            (
                                state.path.with_bike_transfer(
                                    &anchor_stops[bike_start_anchor],
                                    &anchor_stops[from_anchor + 1],
                                    &bike,
                                ),
                                ActiveBikeRun {
                                    start_anchor: from_anchor,
                                    start_stop_id: anchor_stops[from_anchor].clone(),
                                    started_at: bike_started_at,
                                    prefix_path: prefix_path.clone(),
                                    shortcut: bike.clone(),
                                },
                            )
                        };
                        successors.push(EvaluationState {
                            stop_id: anchor_stops[from_anchor + 1].clone(),
                            ready_at,
                            transit_legs: state.transit_legs,
                            last_was_transit: false,
                            active_bike_run: Some(active_bike_run),
                            bike_reuse_available_at: state.bike_reuse_available_at,
                            path,
                        });
                    }
                } else {
                    self.stats.expansion_limit_reached = true;
                    return Ok(ControlFlow::Break(()));
                }
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    fn finalize_corridor(
        &mut self,
        states: Vec<EvaluationState>,
        retained: &StaticCorridor,
        transfer_mismatch: &mut bool,
    ) -> Result<Vec<EvaluatedCandidate>, RoutingError> {
        let mut corridor_candidates = Vec::new();
        for state in states {
            if self
                .query
                .deadline
                .is_some_and(|deadline| deadline.is_elapsed())
            {
                return Err(RoutingError::DeadlineExceeded);
            }
            if state.stop_id != retained.egress_stop_id {
                *transfer_mismatch = true;
                continue;
            }
            for egress in self
                .egress_by_stop
                .get(&retained.egress_stop_id)
                .into_iter()
                .flatten()
            {
                if self
                    .query
                    .deadline
                    .is_some_and(|deadline| deadline.is_elapsed())
                {
                    return Err(RoutingError::DeadlineExceeded);
                }
                let arrival_at =
                    state.ready_at + Duration::seconds(i64::from(egress.walking_seconds));
                if !within_query_bounds(self.query, arrival_at)
                    || self
                        .query
                        .latest_arrival
                        .is_some_and(|latest| arrival_at > latest)
                {
                    continue;
                }
                let candidate = make_candidate(&state.path, egress, arrival_at);
                let Some(variant) = make_variant(candidate.clone(), &state.path.bike_shortcuts)
                else {
                    self.stats.malformed_trips += 1;
                    continue;
                };
                let journey_order_key = candidate.deterministic_order_key();
                let variant_identity = variant.identity();
                corridor_candidates.push(EvaluatedCandidate {
                    metrics: candidate,
                    journey_order_key,
                    bike_share_uses: variant
                        .path()
                        .iter()
                        .filter(|step| {
                            matches!(
                                step.movement,
                                super::variant::JourneyPathMovement::BikeShare { .. }
                            )
                        })
                        .count(),
                    bicycle_seconds: state.path.bicycle_seconds(),
                    variant,
                    variant_identity,
                });
            }
        }

        Ok(corridor_candidates)
    }

    fn finish(
        mut self,
        candidates: Vec<EvaluatedCandidate>,
        started_at: Instant,
    ) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
        if self.stats.expansion_limit_reached {
            self.stats.phase2_latency_us = elapsed_micros(started_at);
            tracing::error!(
                target: "via_server::routing",
                corridors_evaluated = self.stats.corridors_evaluated,
                corridor_states_evaluated = self.stats.corridor_states_evaluated,
                max_expansions = self.query.limits.max_phase2_expansions,
                phase2_latency_us = self.stats.phase2_latency_us,
                "Phase 2 hit its safety bound; rejecting the incomplete candidate set"
            );
            return Err(RoutingError::Phase2EvaluationLimit);
        }

        if self
            .query
            .deadline
            .is_some_and(|deadline| deadline.is_elapsed())
        {
            return Err(RoutingError::DeadlineExceeded);
        }

        self.stats.candidates_before_pareto = candidates.len();
        let candidates = pareto_candidates(candidates, self.query.limits.max_candidates);
        self.stats.final_candidates = candidates.len();
        self.stats.bike_realizations_selected = candidates
            .iter()
            .map(|candidate| {
                candidate
                    .variant
                    .path()
                    .iter()
                    .filter(|step| {
                        matches!(
                            step.movement,
                            super::variant::JourneyPathMovement::BikeShare { .. }
                        )
                    })
                    .count()
            })
            .sum();
        self.stats.phase2_latency_us = elapsed_micros(started_at);
        tracing::debug!(
            target: "via_server::routing",
            corridors_evaluated = self.stats.corridors_evaluated,
            corridors_feasible = self.stats.corridors_feasible,
            corridor_states_evaluated = self.stats.corridor_states_evaluated,
            transit_departure_lookups = self.stats.transit_departure_lookups,
            effective_trips_examined = self.stats.effective_trips_examined,
            surface_transfers_evaluated = self.stats.surface_transfers_evaluated,
            walking_realizations_considered = self.stats.walking_realizations_considered,
            bike_realizations_considered = self.stats.bike_realizations_considered,
            bike_realizations_feasible = self.stats.bike_realizations_feasible,
            bike_realizations_selected = self.stats.bike_realizations_selected,
            infeasible_no_departure = self.stats.infeasible_no_departure,
            infeasible_transfer = self.stats.infeasible_transfer,
            infeasible_access = self.stats.infeasible_access,
            infeasible_constraints = self.stats.infeasible_constraints,
            malformed_trips = self.stats.malformed_trips,
            candidates_before_pareto = self.stats.candidates_before_pareto,
            final_candidates = self.stats.final_candidates,
            phase2_latency_us = self.stats.phase2_latency_us,
            expansion_limit_reached = self.stats.expansion_limit_reached,
            "Phase 2 corridor evaluation"
        );
        Ok((
            candidates
                .into_iter()
                .map(|candidate| candidate.variant)
                .collect(),
            self.stats,
        ))
    }
}

fn charge_state(stats: &mut Phase2EvaluationStats, max_phase2_expansions: usize) -> bool {
    if stats.corridor_states_evaluated >= max_phase2_expansions {
        return true;
    }
    stats.corridor_states_evaluated += 1;
    false
}

fn valid_pattern_leg(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    retained: &StaticTransitLeg,
    alternative: &StaticTransitLeg,
) -> bool {
    if !corridor.is_equivalent_leg(network, retained, alternative) {
        return false;
    }
    let Some(pattern) = network.pattern(&alternative.pattern_id) else {
        return false;
    };
    pattern.stops.get(alternative.board_index) == Some(&retained.board_stop_id)
        && pattern.stops.get(alternative.alight_index) == Some(&retained.alight_stop_id)
        && alternative.board_index < alternative.alight_index
}

fn retained_alight_time(
    network: &TransitRuntime,
    trip: &EffectiveTrip,
    leg: &StaticTransitLeg,
    ready_at: OffsetDateTime,
) -> Option<OffsetDateTime> {
    if trip.pattern_id != leg.pattern_id || leg.board_index >= leg.alight_index {
        return None;
    }
    let pattern = network.pattern(&leg.pattern_id)?;
    if trip.stop_times.len() != pattern.stops.len()
        || trip
            .stop_times
            .iter()
            .zip(&pattern.stops)
            .any(|(time, stop_id)| &time.stop_id != stop_id)
        || trip
            .stop_times
            .iter()
            .any(|time| time.arrival_at > time.departure_at)
        || trip
            .stop_times
            .windows(2)
            .any(|pair| pair[0].departure_at > pair[1].arrival_at)
    {
        return None;
    }
    let board = trip.stop_times.get(leg.board_index)?;
    let alight = trip.stop_times.get(leg.alight_index)?;
    if board.stop_id != leg.board_stop_id
        || alight.stop_id != leg.alight_stop_id
        || board.departure_at < ready_at
        || alight.arrival_at < board.departure_at
    {
        return None;
    }
    Some(alight.arrival_at)
}

fn within_query_bounds(query: &TransitQuery, time: OffsetDateTime) -> bool {
    time >= query.departure_time
        && time - query.departure_time
            <= Duration::seconds(i64::from(query.limits.max_journey_duration_seconds))
}

fn prune_states(mut states: Vec<EvaluationState>, limit: usize) -> Vec<EvaluationState> {
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

fn index_egress(points: &[EgressPoint]) -> HashMap<BoardingPointId, Vec<EgressPoint>> {
    let mut by_stop = HashMap::<BoardingPointId, Vec<EgressPoint>>::new();
    for point in points {
        by_stop
            .entry(point.stop_id.clone())
            .or_default()
            .push(point.clone());
    }
    by_stop
}

fn elapsed_micros(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::{Duration, Time, macros::datetime};

    use super::*;
    use crate::{
        api::GeoCoordinate,
        domain::{
            bikeshare::BikeWalkingAccess,
            ids::{BikeStationId, EffectiveTripId, TransitPatternId},
            transit::{TransferEdge, TransitMode, TransitNetwork, TransitPattern, TransitStop},
            transit_runtime::TransitRuntime,
            transit_timing::{EffectiveStopTime, TimingBasis, TimingReliability},
        },
        routing::{
            AccessPoint, EgressPoint,
            static_search::{StaticCorridor, generate},
            topology::TransitTopologyIndex,
            transfers::TransferShortcutIndex,
        },
    };

    fn stop(id: &str) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(id),
            id,
            GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )
    }

    fn pattern(id: &str, mode: TransitMode, stops: &[&str]) -> TransitPattern {
        TransitPattern::fixture(
            TransitPatternId::new(id),
            mode,
            id,
            stops.last().copied().unwrap_or_default(),
            stops
                .iter()
                .map(|stop| BoardingPointId::new(*stop))
                .collect(),
        )
    }

    fn network(
        patterns: Vec<TransitPattern>,
        stop_ids: &[&str],
        transfers: Vec<TransferEdge>,
    ) -> Arc<TransitRuntime> {
        Arc::new(TransitRuntime::new(Arc::new(
            TransitNetwork::fixture(
                Time::from_hms(4, 0, 0).unwrap(),
                stop_ids.iter().map(|id| stop(id)).collect(),
                patterns,
                Vec::new(),
            )
            .with_transfer_edges(transfers),
        )))
    }

    fn query(modes: &[TransitMode], max_transfers: usize, max_corridors: usize) -> TransitQuery {
        TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("origin"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes(modes.iter().copied())
        .with_limits(crate::routing::PlannerLimits {
            max_transfers,
            max_candidates: 16,
            max_phase2_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
            ..crate::routing::PlannerLimits::default()
        })
        .with_max_static_corridors(max_corridors)
    }

    fn candidate_corridor(network: &TransitRuntime, query: &TransitQuery) -> RetainedCorridorSet {
        let topology = TransitTopologyIndex::new(network.data());
        let transfers = TransferShortcutIndex::from_network(network.data());
        generate(network, &topology, &transfers, query).0
    }

    fn trip(id: &str, pattern_id: &str, stops: &[&str], minute_offsets: &[i64]) -> EffectiveTrip {
        assert_eq!(stops.len(), minute_offsets.len());
        EffectiveTrip {
            id: EffectiveTripId::new(id),
            pattern_id: TransitPatternId::new(pattern_id),
            scheduled_trip_id: None,
            stop_times: stops
                .iter()
                .zip(minute_offsets)
                .map(|(stop, minute)| EffectiveStopTime {
                    stop_id: BoardingPointId::new(*stop),
                    arrival_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(*minute),
                    departure_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(*minute),
                })
                .collect(),
            timing_basis: TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn physical_reboarding_counts_distinct_effective_trips_but_through_service_does_not() {
        let trip_a = trip("trip-a", "red-short", &["R10", "R09"], &[2, 4]);
        let trip_b = trip("trip-b", "red-full", &["R09", "R03"], &[7, 16]);
        let access_at = datetime!(2026-09-21 10:00 UTC);
        let access = AccessPoint::new(BoardingPointId::new("R10"), access_at, access_at, 0, 0);
        let egress = EgressPoint::new(BoardingPointId::new("destination"), 0, 0);
        let boarded_twice = EvaluationPath::from_access(access.clone())
            .with_ride(&trip_a, 0, 1)
            .with_ride(&trip_b, 0, 1);

        let reboarding = make_candidate(&boarded_twice, &egress, datetime!(2026-09-21 10:16 UTC));
        assert_eq!(reboarding.transfer_count, 1);

        let same_physical_trip = trip("through-trip", "red-short", &["R10", "R09"], &[2, 4]);
        let continued_trip = trip("through-trip", "red-full", &["R09", "R03"], &[4, 16]);
        let stayed_aboard = EvaluationPath::from_access(access)
            .with_ride(&same_physical_trip, 0, 1)
            .with_ride(&continued_trip, 0, 1);

        let through = make_candidate(&stayed_aboard, &egress, datetime!(2026-09-21 10:16 UTC));
        assert_eq!(through.transfer_count, 0);
    }

    #[test]
    fn repeated_schedule_basis_does_not_prevent_direct_route_from_dominating_reboarding() {
        let start = datetime!(2026-09-21 10:00 UTC);
        let access = AccessPoint::new(BoardingPointId::new("R10"), start, start, 223, 0);
        let egress = EgressPoint::new(BoardingPointId::new("egress"), 116, 0);
        let terminal_walk = TransferShortcut::new(
            BoardingPointId::new("R03"),
            BoardingPointId::new("egress"),
            86,
            100,
        );
        let arrival = datetime!(2026-09-21 10:19:22 UTC);

        let mut direct_trip = trip("through", "red-full", &["R10", "R03"], &[2, 16]);
        direct_trip.timing_basis = TimingBasis::Schedule;
        direct_trip.timing_reliability = TimingReliability::Fallback;
        let direct_path = EvaluationPath::from_access(access.clone())
            .with_ride(&direct_trip, 0, 1)
            .with_walking_transfer(&terminal_walk);
        let direct_candidate = make_candidate(&direct_path, &egress, arrival);
        let direct_variant = make_variant(direct_candidate.clone(), &[]).unwrap();
        let direct = EvaluatedCandidate {
            journey_order_key: direct_candidate.deterministic_order_key(),
            variant_identity: direct_variant.identity(),
            variant: direct_variant,
            metrics: direct_candidate,
            bike_share_uses: 0,
            bicycle_seconds: 0,
        };

        let mut first_trip = trip("first", "red-short", &["R10", "R09"], &[2, 4]);
        first_trip.timing_basis = TimingBasis::Schedule;
        first_trip.timing_reliability = TimingReliability::Fallback;
        let mut second_trip = trip("second", "red-full", &["R09", "R03"], &[7, 16]);
        second_trip.timing_basis = TimingBasis::Schedule;
        second_trip.timing_reliability = TimingReliability::Fallback;
        let split_path = EvaluationPath::from_access(access)
            .with_ride(&first_trip, 0, 1)
            .with_ride(&second_trip, 0, 1)
            .with_walking_transfer(&terminal_walk);
        let split_candidate = make_candidate(&split_path, &egress, arrival);
        let split_variant = make_variant(split_candidate.clone(), &[]).unwrap();
        let split = EvaluatedCandidate {
            journey_order_key: split_candidate.deterministic_order_key(),
            variant_identity: split_variant.identity(),
            variant: split_variant,
            metrics: split_candidate,
            bike_share_uses: 0,
            bicycle_seconds: 0,
        };

        assert_eq!(direct.metrics.arrival_time, split.metrics.arrival_time);
        assert_eq!(
            direct.metrics.walking_seconds,
            split.metrics.walking_seconds
        );
        assert_eq!(direct.metrics.transfer_count, 0);
        assert_eq!(split.metrics.transfer_count, 1);
        assert!(direct.dominates(&split));
    }

    fn static_leg(pattern_id: &str, board: &str, alight: &str) -> StaticCorridorStep {
        StaticCorridorStep::Transit(StaticTransitLeg {
            pattern_id: TransitPatternId::new(pattern_id),
            board_stop_id: BoardingPointId::new(board),
            board_index: 0,
            alight_stop_id: BoardingPointId::new(alight),
            alight_index: 1,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
        })
    }

    fn corridor_movement(
        from_anchor: usize,
        to_anchor: usize,
        from: &str,
        to: &str,
    ) -> CorridorMovement {
        CorridorMovement {
            from_anchor,
            to_anchor,
            from: BoardingPointId::new(from),
            to: BoardingPointId::new(to),
        }
    }

    fn bike_shortcut(from_anchor: usize, to_anchor: usize, seconds: u32) -> BikeShortcut {
        BikeShortcut::new(
            from_anchor,
            to_anchor,
            BikeStationId::new(format!("pickup-{from_anchor}")),
            BikeStationId::new(format!("dropoff-{to_anchor}")),
            BikeWalkingAccess::new(0, 0),
            crate::routing::BikeRide {
                duration_seconds: seconds,
                distance_metres: seconds,
            },
            BikeWalkingAccess::new(0, 0),
        )
        .unwrap()
    }

    #[test]
    fn phase2_single_leg_route() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip(
                "bus-trip",
                "bus",
                &["origin", "destination"],
                &[1, 20],
            )],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:20 UTC)
        );
        assert_eq!(stats.corridors_feasible, 1);
    }

    #[test]
    fn phase2_multi_leg_route_keeps_explicit_surface_transfer() {
        let network = network(
            vec![
                pattern("bus", TransitMode::Bus, &["origin", "bus-end"]),
                pattern("metro", TransitMode::Metro, &["metro-start", "destination"]),
            ],
            &["origin", "bus-end", "metro-start", "destination"],
            vec![TransferEdge::new(
                BoardingPointId::new("bus-end"),
                BoardingPointId::new("metro-start"),
                30,
                100,
            )],
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip("bus-trip", "bus", &["origin", "bus-end"], &[1, 9])],
        );
        snapshot.insert(
            TransitPatternId::new("metro"),
            0,
            vec![trip(
                "metro-trip",
                "metro",
                &["metro-start", "destination"],
                &[10, 20],
            )],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].evaluate_timing().unwrap().walking_seconds, 30);
        assert!(matches!(
            candidates[0]
                .skeleton()
                .movements()
                .get(2)
                .map(|movement| &movement.kind),
            Some(super::super::variant::JourneyMovementKind::Walking {
                kind: super::super::variant::WalkingMovementKind::Transfer,
                ..
            })
        ));
    }

    #[test]
    fn faster_bike_surface_transfer_catches_the_next_transit_departure() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "transfer-from"]),
                pattern(
                    "second",
                    TransitMode::Metro,
                    &["transfer-to", "destination"],
                ),
            ],
            &["origin", "transfer-from", "transfer-to", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 1);
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 0,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("first", "origin", "transfer-from"),
                    StaticCorridorStep::SurfaceTransfer {
                        from: BoardingPointId::new("transfer-from"),
                        to: BoardingPointId::new("transfer-to"),
                        walking_duration_seconds: 8 * 60,
                        walking_distance_metres: 100,
                    },
                    static_leg("second", "transfer-to", "destination"),
                ],
            }],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip(
                "first-trip",
                "first",
                &["origin", "transfer-from"],
                &[0, 0],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip(
                "second-trip",
                "second",
                &["transfer-to", "destination"],
                &[6, 10],
            )],
        );
        let movement = CorridorMovement {
            from_anchor: 1,
            to_anchor: 2,
            from: BoardingPointId::new("transfer-from"),
            to: BoardingPointId::new("transfer-to"),
        };
        let bike = BikeShortcut::new(
            1,
            2,
            BikeStationId::new("pickup"),
            BikeStationId::new("dropoff"),
            BikeWalkingAccess::new(60, 50),
            crate::routing::BikeRide {
                duration_seconds: 200,
                distance_metres: 1_000,
            },
            BikeWalkingAccess::new(90, 50),
        )
        .unwrap();
        let realizations = HashMap::from([(movement, vec![bike])]);

        let (variants, stats) =
            evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
                .unwrap();

        assert_eq!(variants.len(), 1);
        let timing = variants[0].evaluate_timing().unwrap();
        assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:10 UTC));
        assert_eq!(timing.bike_share_uses, 1);
        assert_eq!(timing.walking_seconds, 150);
        let bike_shortcut = variants[0]
            .path()
            .iter()
            .find_map(|step| match &step.movement {
                super::super::variant::JourneyPathMovement::BikeShare { shortcut } => {
                    Some(shortcut)
                }
                super::super::variant::JourneyPathMovement::Baseline { .. } => None,
            })
            .unwrap();
        assert_eq!(bike_shortcut.walk_to_pickup.duration_seconds, 60);
        assert_eq!(bike_shortcut.walk_from_dropoff.duration_seconds, 90);
        let bike_step = variants[0]
            .path()
            .iter()
            .find(|step| {
                matches!(
                    step.movement,
                    super::super::variant::JourneyPathMovement::BikeShare { .. }
                )
            })
            .unwrap();
        assert!(matches!(
            &variants[0].skeleton().anchors()[bike_step.from_anchor].kind,
            super::super::variant::JourneyAnchorKind::BoardingPoint { id }
                if id == &BoardingPointId::new("transfer-from")
        ));
        assert!(matches!(
            &variants[0].skeleton().anchors()[bike_step.to_anchor].kind,
            super::super::variant::JourneyAnchorKind::BoardingPoint { id }
                if id == &BoardingPointId::new("transfer-to")
        ));
        assert_eq!(stats.bike_realizations_selected, 1);
        assert!(variants[0].path().iter().any(|step| matches!(
            step.movement,
            super::super::variant::JourneyPathMovement::BikeShare { .. }
        )));
    }

    #[test]
    fn bike_replacing_a_transit_leg_requeries_the_next_departure() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "middle"]),
                pattern("second", TransitMode::Metro, &["middle", "destination"]),
            ],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 1);
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 0,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("first", "origin", "middle"),
                    static_leg("second", "middle", "destination"),
                ],
            }],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip("first-trip", "first", &["origin", "middle"], &[1, 20])],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![
                trip("second-17", "second", &["middle", "destination"], &[17, 27]),
                trip("second-25", "second", &["middle", "destination"], &[25, 35]),
            ],
        );
        let movement = corridor_movement(0, 1, "origin", "middle");
        let realizations = HashMap::from([(movement, vec![bike_shortcut(0, 1, 16 * 60)])]);

        let (variants, _) =
            evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
                .unwrap();

        let bike_variant = variants
            .iter()
            .find(|variant| {
                variant.path().iter().any(|step| {
                    matches!(
                        step.movement,
                        super::super::variant::JourneyPathMovement::BikeShare { .. }
                    )
                })
            })
            .unwrap();
        assert_eq!(
            bike_variant.evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:27 UTC)
        );
        assert_eq!(
            bike_variant.transit_pattern_sequence(),
            vec![TransitPatternId::new("second")]
        );
        assert!(bike_variant.skeleton().movements().iter().any(|movement| {
            matches!(
                &movement.kind,
                super::super::variant::JourneyMovementKind::Transit { trip, .. }
                    if trip.id == EffectiveTripId::new("second-17")
            )
        }));
    }

    #[test]
    fn one_bike_shortcut_can_skip_multiple_transit_anchors() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "middle"]),
                pattern("second", TransitMode::Metro, &["middle", "destination"]),
            ],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 1);
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 0,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("first", "origin", "middle"),
                    static_leg("second", "middle", "destination"),
                ],
            }],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip("first-trip", "first", &["origin", "middle"], &[1, 10])],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip(
                "second-trip",
                "second",
                &["middle", "destination"],
                &[40, 50],
            )],
        );
        let realizations = HashMap::from([
            (
                corridor_movement(0, 1, "origin", "middle"),
                vec![bike_shortcut(0, 1, 5 * 60)],
            ),
            (
                corridor_movement(0, 2, "origin", "destination"),
                vec![bike_shortcut(0, 2, 15 * 60)],
            ),
        ]);

        let (variants, stats) =
            evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
                .unwrap();

        let cross_anchor = variants
            .iter()
            .find(|variant| {
                variant.transit_pattern_sequence().is_empty()
                    && variant.path().iter().any(|step| {
                        matches!(
                            step.movement,
                            super::super::variant::JourneyPathMovement::BikeShare { .. }
                        )
                    })
            })
            .unwrap();
        let timing = cross_anchor.evaluate_timing().unwrap();
        assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:15 UTC));
        assert_eq!(timing.bike_share_uses, 1);
        assert_eq!(stats.bike_exact_candidates_requested, 2);
        assert_eq!(stats.bike_continuous_extensions, 1);
    }

    #[test]
    fn bike_can_replace_a_surface_walking_edge() {
        let network = network(Vec::new(), &["a", "b"], Vec::new());
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        );
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("b"),
                theoretical_duration_seconds: 600,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![StaticCorridorStep::SurfaceTransfer {
                    from: BoardingPointId::new("a"),
                    to: BoardingPointId::new("b"),
                    walking_duration_seconds: 600,
                    walking_distance_metres: 100,
                }],
            }],
        );
        let realizations = HashMap::from([(
            corridor_movement(0, 1, "a", "b"),
            vec![bike_shortcut(0, 1, 300)],
        )]);

        let variants = evaluate_with_test_bike_routes(
            &network,
            &corridor,
            &TimingSnapshot::default(),
            query,
            &realizations,
        )
        .unwrap()
        .0;

        assert!(variants.iter().any(|variant| {
            variant.path().iter().any(|step| {
                matches!(
                    step.movement,
                    super::super::variant::JourneyPathMovement::BikeShare { .. }
                )
            })
        }));
    }

    #[test]
    fn slower_optimistic_bike_bound_skips_exact_routing() {
        let network = network(Vec::new(), &["a", "b"], Vec::new());
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        );
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("b"),
                theoretical_duration_seconds: 300,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![StaticCorridorStep::SurfaceTransfer {
                    from: BoardingPointId::new("a"),
                    to: BoardingPointId::new("b"),
                    walking_duration_seconds: 300,
                    walking_distance_metres: 100,
                }],
            }],
        );
        let realizations = HashMap::from([(
            corridor_movement(0, 1, "a", "b"),
            vec![bike_shortcut(0, 1, 20 * 60)],
        )]);

        let (variants, stats) = evaluate_with_test_bike_routes(
            &network,
            &corridor,
            &TimingSnapshot::default(),
            query,
            &realizations,
        )
        .unwrap();

        assert_eq!(stats.bike_lower_bound_checks, 1);
        assert_eq!(stats.bike_lower_bound_rejections, 1);
        assert_eq!(stats.bike_exact_candidates_requested, 0);
        assert!(
            variants
                .iter()
                .all(|variant| variant.path().iter().all(|step| !matches!(
                    step.movement,
                    super::super::variant::JourneyPathMovement::BikeShare { .. }
                )))
        );
    }

    #[test]
    fn transit_waiting_time_is_included_when_screening_bike() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 1);
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 4 * 60,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![static_leg("bus", "origin", "destination")],
            }],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip(
                "bus-trip",
                "bus",
                &["origin", "destination"],
                &[7, 11],
            )],
        );
        let realizations = HashMap::from([(
            corridor_movement(0, 1, "origin", "destination"),
            vec![bike_shortcut(0, 1, 16 * 60)],
        )]);

        let (variants, stats) =
            evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
                .unwrap();

        assert_eq!(
            variants[0].evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:11 UTC)
        );
        assert_eq!(stats.bike_lower_bound_checks, 1);
        assert_eq!(stats.bike_exact_candidates_requested, 1);
        assert_eq!(stats.bike_realizations_feasible, 0);
    }

    #[test]
    fn exact_bike_that_loses_to_walking_is_not_retained() {
        let network = network(Vec::new(), &["a", "b"], Vec::new());
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        );
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("b"),
                theoretical_duration_seconds: 12 * 60,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![StaticCorridorStep::SurfaceTransfer {
                    from: BoardingPointId::new("a"),
                    to: BoardingPointId::new("b"),
                    walking_duration_seconds: 12 * 60,
                    walking_distance_metres: 100,
                }],
            }],
        );
        let realizations = HashMap::from([(
            corridor_movement(0, 1, "a", "b"),
            vec![bike_shortcut(0, 1, 14 * 60)],
        )]);

        let (variants, stats) = evaluate_with_test_bike_routes(
            &network,
            &corridor,
            &TimingSnapshot::default(),
            query,
            &realizations,
        )
        .unwrap();

        assert_eq!(stats.bike_exact_candidates_requested, 1);
        assert_eq!(stats.bike_realizations_feasible, 0);
        assert!(
            variants
                .iter()
                .all(|variant| variant.path().iter().all(|step| !matches!(
                    step.movement,
                    super::super::variant::JourneyPathMovement::BikeShare { .. }
                )))
        );
    }

    #[test]
    fn identical_physical_bike_movements_use_one_exact_realization() {
        let network = network(Vec::new(), &["a", "b"], Vec::new());
        let query = TransitQuery::new(
            vec![
                AccessPoint::new(
                    BoardingPointId::new("a"),
                    datetime!(2026-09-21 10:00 UTC),
                    datetime!(2026-09-21 10:00 UTC),
                    0,
                    0,
                ),
                AccessPoint::new(
                    BoardingPointId::new("a"),
                    datetime!(2026-09-21 10:00 UTC),
                    datetime!(2026-09-21 10:00 UTC),
                    0,
                    10,
                ),
            ],
            vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        );
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("b"),
                theoretical_duration_seconds: 10 * 60,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![StaticCorridorStep::SurfaceTransfer {
                    from: BoardingPointId::new("a"),
                    to: BoardingPointId::new("b"),
                    walking_duration_seconds: 10 * 60,
                    walking_distance_metres: 100,
                }],
            }],
        );
        let realizations = HashMap::from([(
            corridor_movement(0, 1, "a", "b"),
            vec![bike_shortcut(0, 1, 5 * 60)],
        )]);

        let (_, stats) = evaluate_with_test_bike_routes(
            &network,
            &corridor,
            &TimingSnapshot::default(),
            query,
            &realizations,
        )
        .unwrap();

        assert_eq!(stats.bike_exact_candidates_requested, 1);
        assert_eq!(stats.bike_exact_cache_hits, 1);
    }

    #[test]
    fn long_corridor_only_checks_its_next_edges_before_exact_routing() {
        let stop_names = (0..=12)
            .map(|index| format!("stop-{index}"))
            .collect::<Vec<_>>();
        let stop_refs = stop_names.iter().map(String::as_str).collect::<Vec<_>>();
        let network = network(Vec::new(), &stop_refs, Vec::new());
        let departure_at = datetime!(2026-09-21 10:00 UTC);
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new(&stop_names[0]),
                departure_at,
                departure_at,
                0,
                0,
            )],
            vec![EgressPoint::new(
                BoardingPointId::new(stop_names.last().unwrap()),
                0,
                0,
            )],
            departure_at,
        );
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new(&stop_names[0]),
                egress_stop_id: BoardingPointId::new(stop_names.last().unwrap()),
                theoretical_duration_seconds: 12 * 60,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: stop_names
                    .windows(2)
                    .map(|pair| StaticCorridorStep::SurfaceTransfer {
                        from: BoardingPointId::new(&pair[0]),
                        to: BoardingPointId::new(&pair[1]),
                        walking_duration_seconds: 60,
                        walking_distance_metres: 100,
                    })
                    .collect(),
            }],
        );
        let realizations = (0..stop_names.len() - 1)
            .flat_map(|from| {
                let stop_names = &stop_names;
                (from + 1..stop_names.len()).map(move |to| {
                    (
                        corridor_movement(from, to, &stop_names[from], &stop_names[to]),
                        vec![bike_shortcut(from, to, 20 * 60)],
                    )
                })
            })
            .collect::<HashMap<_, _>>();

        let (_, stats) = evaluate_with_test_bike_routes(
            &network,
            &corridor,
            &TimingSnapshot::default(),
            query,
            &realizations,
        )
        .unwrap();

        assert_eq!(stats.bike_lower_bound_checks, 12);
        assert_eq!(stats.bike_lower_bound_rejections, 12);
        assert_eq!(stats.bike_exact_candidates_requested, 0);
    }

    #[test]
    fn separate_bike_rental_waits_until_fifteen_minutes_after_return() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["a", "b"]),
                pattern("second", TransitMode::Metro, &["b", "c"]),
                pattern("third", TransitMode::Bus, &["c", "d"]),
            ],
            &["a", "b", "c", "d"],
            Vec::new(),
        );
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("d"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes([TransitMode::Bus, TransitMode::Metro])
        .with_limits(crate::routing::PlannerLimits {
            max_transfers: 2,
            max_candidates: 16,
            max_phase2_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
            ..crate::routing::PlannerLimits::default()
        });
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("d"),
                theoretical_duration_seconds: 0,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("first", "a", "b"),
                    static_leg("second", "b", "c"),
                    static_leg("third", "c", "d"),
                ],
            }],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip("first-trip", "first", &["a", "b"], &[1, 30])],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip("second-trip", "second", &["b", "c"], &[11, 15])],
        );
        snapshot.insert(
            TransitPatternId::new("third"),
            0,
            vec![trip("third-trip", "third", &["c", "d"], &[30, 50])],
        );
        let first_bike = BikeShortcut::new(
            0,
            1,
            BikeStationId::new("pickup-a"),
            BikeStationId::new("dropoff-b"),
            BikeWalkingAccess::new(0, 0),
            crate::routing::BikeRide {
                duration_seconds: 5 * 60,
                distance_metres: 1_000,
            },
            BikeWalkingAccess::new(5 * 60, 100),
        )
        .unwrap();
        let second_bike = bike_shortcut(2, 3, 5 * 60);
        let realizations = HashMap::from([
            (corridor_movement(0, 1, "a", "b"), vec![first_bike]),
            (corridor_movement(2, 3, "c", "d"), vec![second_bike]),
        ]);

        let (variants, stats) =
            evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
                .unwrap();

        let split_rentals = variants
            .iter()
            .find(|variant| {
                variant
                    .path()
                    .iter()
                    .filter(|step| {
                        matches!(
                            step.movement,
                            super::super::variant::JourneyPathMovement::BikeShare { .. }
                        )
                    })
                    .count()
                    == 2
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected two rentals; stats={:?}, variants={:?}",
                    stats,
                    variants
                        .iter()
                        .map(|variant| variant.identity().canonical_signature())
                        .collect::<Vec<_>>()
                )
            });
        let timing = split_rentals.evaluate_timing().unwrap();
        assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:25 UTC));
        assert_eq!(timing.bike_share_uses, 2);
        let last_bike = split_rentals
            .path()
            .iter()
            .rev()
            .find(|step| {
                matches!(
                    step.movement,
                    super::super::variant::JourneyPathMovement::BikeShare { .. }
                )
            })
            .unwrap();
        assert!(matches!(
            &last_bike.movement,
            super::super::variant::JourneyPathMovement::BikeShare { shortcut }
                if shortcut.rental_wait_seconds == 5 * 60
        ));
    }

    #[test]
    fn bike_with_no_pareto_benefit_is_pruned() {
        let network = network(Vec::new(), &["a", "b"], Vec::new());
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        );
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("b"),
                theoretical_duration_seconds: 60,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![StaticCorridorStep::SurfaceTransfer {
                    from: BoardingPointId::new("a"),
                    to: BoardingPointId::new("b"),
                    walking_duration_seconds: 60,
                    walking_distance_metres: 100,
                }],
            }],
        );
        let bike = BikeShortcut::new(
            0,
            1,
            BikeStationId::new("pickup"),
            BikeStationId::new("dropoff"),
            BikeWalkingAccess::new(30, 15),
            crate::routing::BikeRide {
                duration_seconds: 30,
                distance_metres: 100,
            },
            BikeWalkingAccess::new(30, 15),
        )
        .unwrap();
        let realizations = HashMap::from([(corridor_movement(0, 1, "a", "b"), vec![bike])]);

        let variants = evaluate_with_test_bike_routes(
            &network,
            &corridor,
            &TimingSnapshot::default(),
            query,
            &realizations,
        )
        .unwrap()
        .0;

        assert!(
            variants
                .iter()
                .all(|variant| variant.path().iter().all(|step| !matches!(
                    step.movement,
                    super::super::variant::JourneyPathMovement::BikeShare { .. }
                )))
        );
    }

    #[test]
    fn transit_remains_when_it_reaches_the_anchor_before_bike() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["a", "b"])],
            &["a", "b"],
            Vec::new(),
        );
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes([TransitMode::Bus]);
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("a"),
                egress_stop_id: BoardingPointId::new("b"),
                theoretical_duration_seconds: 0,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![static_leg("bus", "a", "b")],
            }],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip("bus-trip", "bus", &["a", "b"], &[1, 5])],
        );
        let realizations = HashMap::from([(
            corridor_movement(0, 1, "a", "b"),
            vec![bike_shortcut(0, 1, 10 * 60)],
        )]);

        let variants =
            evaluate_with_test_bike_routes(&network, &corridor, &snapshot, query, &realizations)
                .unwrap()
                .0;

        assert_eq!(variants.len(), 1);
        assert!(
            variants[0]
                .transit_pattern_sequence()
                .contains(&TransitPatternId::new("bus"))
        );
        assert!(variants[0].path().iter().all(|step| !matches!(
            step.movement,
            super::super::variant::JourneyPathMovement::BikeShare { .. }
        )));
    }

    #[test]
    fn pareto_keeps_a_later_route_with_less_walking() {
        let network = network(
            vec![
                pattern("walky", TransitMode::Bus, &["a", "destination"]),
                pattern("lowwalk", TransitMode::Bus, &["x", "destination"]),
            ],
            &["a", "x", "destination"],
            Vec::new(),
        );
        let mut query = TransitQuery::new(
            vec![
                AccessPoint::new(
                    BoardingPointId::new("a"),
                    datetime!(2026-09-21 10:00 UTC),
                    datetime!(2026-09-21 10:02 UTC),
                    120,
                    600,
                ),
                AccessPoint::new(
                    BoardingPointId::new("x"),
                    datetime!(2026-09-21 10:00 UTC),
                    datetime!(2026-09-21 10:00 UTC),
                    0,
                    0,
                ),
            ],
            vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        );
        query.limits.max_candidates = 10;
        let corridor = RetainedCorridorSet::from_retained_for_test(
            &network,
            vec![
                StaticCorridor {
                    access_stop_id: BoardingPointId::new("a"),
                    egress_stop_id: BoardingPointId::new("destination"),
                    theoretical_duration_seconds: 300,
                    cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                    steps: vec![static_leg("walky", "a", "destination")],
                },
                StaticCorridor {
                    access_stop_id: BoardingPointId::new("x"),
                    egress_stop_id: BoardingPointId::new("destination"),
                    theoretical_duration_seconds: 480,
                    cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                    steps: vec![static_leg("lowwalk", "x", "destination")],
                },
            ],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("walky"),
            0,
            vec![trip("walky-trip", "walky", &["a", "destination"], &[2, 5])],
        );
        snapshot.insert(
            TransitPatternId::new("lowwalk"),
            0,
            vec![trip(
                "lowwalk-trip",
                "lowwalk",
                &["x", "destination"],
                &[4, 8],
            )],
        );

        let variants = evaluate_with_test_bike_routes(
            &network,
            &corridor,
            &snapshot,
            query,
            &TestBikeRoutes::new(),
        )
        .unwrap()
        .0;

        assert_eq!(variants.len(), 2);
        let metrics = variants
            .iter()
            .map(|variant| variant.evaluate_timing().unwrap())
            .collect::<Vec<_>>();
        assert!(
            metrics
                .iter()
                .any(|item| item.arrival_at == datetime!(2026-09-21 10:05 UTC)
                    && item.walking_seconds == 120)
        );
        assert!(
            metrics
                .iter()
                .any(|item| item.arrival_at == datetime!(2026-09-21 10:08 UTC)
                    && item.walking_seconds == 0)
        );
    }

    #[test]
    fn phase2_rejects_corridor_without_a_departure_after_ready_at() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let snapshot = TimingSnapshot::default();

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.is_empty());
        assert_eq!(stats.infeasible_no_departure, 1);
    }

    #[test]
    fn phase2_rejects_non_monotonic_effective_trip_times() {
        let network = network(
            vec![pattern(
                "bus",
                TransitMode::Bus,
                &["origin", "middle", "destination"],
            )],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip(
                "malformed-trip",
                "bus",
                &["origin", "middle", "destination"],
                &[1, 5, 4],
            )],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.is_empty());
        assert!(stats.malformed_trips > 0);
    }

    #[test]
    fn phase2_later_departure_can_have_earlier_arrival() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![
                trip("early-slow", "bus", &["origin", "destination"], &[1, 30]),
                trip("late-fast", "bus", &["origin", "destination"], &[4, 15]),
            ],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:15 UTC)
        );
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![TransitPatternId::new("bus")]
        );
        assert!(matches!(
            candidates[0]
                .skeleton()
                .movements()
                .get(1)
                .map(|movement| &movement.kind),
            Some(super::super::variant::JourneyMovementKind::Transit { .. })
        ));
    }

    #[test]
    fn phase2_keeps_reliable_candidate_when_fallback_arrives_earlier() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut fallback = trip("a-fallback", "bus", &["origin", "destination"], &[1, 5]);
        fallback.timing_reliability = TimingReliability::Fallback;
        let reliable = trip("z-reliable", "bus", &["origin", "destination"], &[2, 6]);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(TransitPatternId::new("bus"), 0, vec![fallback, reliable]);

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.skeleton().movements().iter().any(|movement| {
                matches!(
                    &movement.kind,
                    super::super::variant::JourneyMovementKind::Transit { trip, .. }
                        if trip.timing_reliability == TimingReliability::Reliable
                )
            })
        }));
    }

    #[test]
    fn phase2_state_pruning_keeps_reliability_alternatives_for_later_legs() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "transfer"]),
                pattern("second", TransitMode::Bus, &["transfer", "destination"]),
            ],
            &["origin", "transfer", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut fallback = trip("a-fallback", "first", &["origin", "transfer"], &[1, 3]);
        fallback.timing_reliability = TimingReliability::Fallback;
        let reliable = trip("z-reliable", "first", &["origin", "transfer"], &[1, 3]);
        let second = trip(
            "second-trip",
            "second",
            &["transfer", "destination"],
            &[5, 10],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(TransitPatternId::new("first"), 0, vec![fallback, reliable]);
        snapshot.insert(TransitPatternId::new("second"), 0, vec![second]);

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.skeleton().movements().iter().any(|movement| {
                matches!(
                    &movement.kind,
                    super::super::variant::JourneyMovementKind::Transit { trip, .. }
                        if trip.id == EffectiveTripId::new("z-reliable")
                )
            })
        }));
    }

    #[test]
    fn phase2_respects_maximum_journey_duration_and_latest_arrival() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let corridor_query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &corridor_query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip(
                "late-trip",
                "bus",
                &["origin", "destination"],
                &[1, 20],
            )],
        );

        let short_duration_query =
            corridor_query
                .clone()
                .with_limits(crate::routing::PlannerLimits {
                    max_transfers: 0,
                    max_candidates: 16,
                    max_phase2_expansions: 2_000,
                    max_departures_per_query: 3,
                    max_journey_duration_seconds: 10 * 60,
                    ..crate::routing::PlannerLimits::default()
                });
        assert!(
            evaluate(&network, &corridor, &snapshot, short_duration_query)
                .unwrap()
                .is_empty()
        );

        let mut latest_arrival_query = corridor_query;
        latest_arrival_query.latest_arrival = Some(datetime!(2026-09-21 10:10 UTC));
        assert!(
            evaluate(&network, &corridor, &snapshot, latest_arrival_query)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn phase2_respects_candidate_limit_after_pareto_pruning() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let mut query = query(&[TransitMode::Bus], 0, 10);
        query.origin_access[0].walking_seconds = 200;
        query.origin_access.push(AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:03 UTC),
            100,
            0,
        ));
        query.origin_access.push(AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:05 UTC),
            0,
            0,
        ));
        query.limits.max_candidates = 2;
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![
                trip("early-arrival", "bus", &["origin", "destination"], &[1, 5]),
                trip("middle-arrival", "bus", &["origin", "destination"], &[4, 8]),
                trip("late-arrival", "bus", &["origin", "destination"], &[6, 10]),
            ],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(stats.final_candidates, 2);
        assert!(stats.candidates_before_pareto >= stats.final_candidates);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn phase2_expansion_limit_rejects_partial_candidates() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let mut query = query(&[TransitMode::Bus], 0, 10);
        query.origin_access.push(AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        ));
        query.limits.max_phase2_expansions = 1;
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip("bus-trip", "bus", &["origin", "destination"], &[1, 5])],
        );

        assert!(evaluate_with_stats(&network, &corridor, &snapshot, query).is_err());
    }

    #[test]
    fn phase2_applies_same_stop_transfer_time() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "middle"]),
                pattern("second", TransitMode::Metro, &["middle", "destination"]),
            ],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip("first-trip", "first", &["origin", "middle"], &[1, 10])],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip(
                "too-soon",
                "second",
                &["middle", "destination"],
                &[11, 20],
            )],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.is_empty());
        assert!(network.same_stop_transfer_seconds() >= 60);
        assert_eq!(stats.corridors_feasible, 0);
        assert!(stats.transit_departure_lookups > 0);
    }

    #[test]
    fn explicit_surface_transfer_does_not_add_same_stop_penalty() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "bus-end"]),
                pattern(
                    "second",
                    TransitMode::Metro,
                    &["metro-start", "destination"],
                ),
            ],
            &["origin", "bus-end", "metro-start", "destination"],
            vec![TransferEdge::new(
                BoardingPointId::new("bus-end"),
                BoardingPointId::new("metro-start"),
                30,
                100,
            )],
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip("first-trip", "first", &["origin", "bus-end"], &[1, 9])],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip(
                "second-trip",
                "second",
                &["metro-start", "destination"],
                &[10, 20],
            )],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:20 UTC)
        );
    }

    #[test]
    fn phase2_allows_equivalent_pattern_substitution() {
        let network = network(
            vec![
                pattern("pattern-a", TransitMode::Bus, &["origin", "destination"]),
                pattern("pattern-b", TransitMode::Bus, &["origin", "destination"]),
            ],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 1);
        let corridor = candidate_corridor(&network, &query);
        let retained_leg = corridor.retained()[0].transit_legs().next().unwrap();
        let alternatives = corridor.equivalent_legs_for(&network, retained_leg);
        assert_eq!(alternatives.len(), 2);
        let alternative = alternatives
            .iter()
            .find(|leg| leg.pattern_id != retained_leg.pattern_id)
            .unwrap();
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            alternative.pattern_id.clone(),
            alternative.board_index,
            vec![trip(
                "equivalent-trip",
                alternative.pattern_id.as_str(),
                &["origin", "destination"],
                &[1, 10],
            )],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![alternative.pattern_id.clone()]
        );
    }

    #[test]
    fn phase2_never_combines_different_retained_corridors() {
        let network = network(
            vec![
                pattern("a-bus-prefix", TransitMode::Bus, &["origin", "middle"]),
                pattern("b-bus-suffix", TransitMode::Bus, &["middle", "destination"]),
                pattern("c-metro-prefix", TransitMode::Metro, &["origin", "middle"]),
                pattern(
                    "d-metro-suffix",
                    TransitMode::Metro,
                    &["middle", "destination"],
                ),
            ],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 2);
        let retained = vec![
            StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 1,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("a-bus-prefix", "origin", "middle"),
                    static_leg("b-bus-suffix", "middle", "destination"),
                ],
            },
            StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 2,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("c-metro-prefix", "origin", "middle"),
                    static_leg("d-metro-suffix", "middle", "destination"),
                ],
            },
        ];
        let corridor = RetainedCorridorSet::from_retained_for_test(&network, retained);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("a-bus-prefix"),
            0,
            vec![trip(
                "a-prefix-trip",
                "a-bus-prefix",
                &["origin", "middle"],
                &[1, 5],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("b-bus-suffix"),
            0,
            vec![trip(
                "b-suffix-trip",
                "b-bus-suffix",
                &["middle", "destination"],
                &[16, 30],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("c-metro-prefix"),
            0,
            vec![trip(
                "z-prefix-trip",
                "c-metro-prefix",
                &["origin", "middle"],
                &[1, 5],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("d-metro-suffix"),
            0,
            vec![trip(
                "d-suffix-trip",
                "d-metro-suffix",
                &["middle", "destination"],
                &[10, 18],
            )],
        );

        let (candidates, _) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.iter().all(|candidate| {
            let sequence = candidate.transit_pattern_sequence();
            sequence
                == [
                    TransitPatternId::new("a-bus-prefix"),
                    TransitPatternId::new("b-bus-suffix"),
                ]
                || sequence
                    == [
                        TransitPatternId::new("c-metro-prefix"),
                        TransitPatternId::new("d-metro-suffix"),
                    ]
        }));
        assert!(!candidates.is_empty());
    }

    #[test]
    fn expired_deadline_stops_phase2_before_evaluating_corridors() {
        let network = network(Vec::new(), &["origin", "destination"], Vec::new());
        let corridor = RetainedCorridorSet::default();
        let snapshot = TimingSnapshot::default();
        let query = query(&[TransitMode::Bus], 0, 1)
            .with_deadline(super::super::model::PlanningDeadline::elapsed_for_test());

        let result = evaluate_with_stats(&network, &corridor, &snapshot, query);

        assert_eq!(result.unwrap_err(), RoutingError::DeadlineExceeded);
    }
}
