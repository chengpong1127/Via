mod bike;
mod pareto;
mod state;
#[cfg(test)]
mod tests;
mod transit;

use self::{
    pareto::{EvaluatedCandidate, make_candidate, make_variant, pareto_candidates, prune_states},
    state::{ActiveBikeRun, EvaluationPath, EvaluationState, bike_start},
};
#[cfg(test)]
use state::{CorridorMovement, TestBikeRoutes};
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
    static_search::{
        CorridorAnchors, RetainedCorridorSet, StaticCorridor, StaticCorridorStep, StaticTransitLeg,
    },
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
        let Some(anchors): Option<CorridorAnchors> = retained.anchors() else {
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

fn within_query_bounds(query: &TransitQuery, time: OffsetDateTime) -> bool {
    time >= query.departure_time
        && time - query.departure_time
            <= Duration::seconds(i64::from(query.limits.max_journey_duration_seconds))
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
