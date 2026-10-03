mod pareto;
mod state;
#[cfg(test)]
mod tests;
mod transit;

use super::{
    corridor::{BaselineMovement, EvaluationCorridor},
    model::{RoutingError, TransitQuery},
    static_search::{RetainedCorridorSet, StaticTransitLeg},
    surface::{SurfaceKind, SurfaceMovement, SurfaceMovementPlanner, SurfaceRequest},
    variant::{
        BikeShortcut, JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
        JourneyVariant, WalkingMovementKind,
    },
};
#[cfg(test)]
use crate::domain::ids::BoardingPointId;
use crate::{
    domain::{
        realtime::NormalizedRealtimeValue,
        transit_runtime::TransitRuntime,
        transit_timing::{EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot},
    },
    street::ProviderFailureClass,
};
use pareto::{EvaluatedCandidate, pareto_candidates, prune_states};
use state::{ActiveBikeRun, EvaluationState, PathMetrics};
use std::{sync::Arc, time::Instant};
use time::{Duration, OffsetDateTime};
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

#[derive(Default, Clone, Copy)]
pub(crate) struct EvaluationFailures {
    pub transient: bool,
    pub internal: bool,
}

pub(super) struct EvaluationResult {
    pub variants: Vec<JourneyVariant>,
    pub stats: Phase2EvaluationStats,
    pub failures: EvaluationFailures,
}

/// One request-local evaluator is retained across transit evaluation and fallback,
/// preserving movement caches and the total expansion budget.
pub(super) struct FixedCorridorEvaluator<'a> {
    network: &'a TransitRuntime,
    corridor: &'a RetainedCorridorSet,
    snapshot: &'a TimingSnapshot,
    query: &'a TransitQuery,
    surface: &'a mut [Box<dyn SurfaceMovementPlanner>],
    pub(super) stats: Phase2EvaluationStats,
    pub(super) failures: EvaluationFailures,
}

impl<'a> FixedCorridorEvaluator<'a> {
    pub fn new(
        network: &'a TransitRuntime,
        corridor: &'a RetainedCorridorSet,
        snapshot: &'a TimingSnapshot,
        query: &'a TransitQuery,
        surface: &'a mut [Box<dyn SurfaceMovementPlanner>],
    ) -> Self {
        Self {
            network,
            corridor,
            snapshot,
            query,
            surface,
            stats: Phase2EvaluationStats::default(),
            failures: EvaluationFailures::default(),
        }
    }
    pub async fn evaluate(
        &mut self,
        corridors: &[EvaluationCorridor],
    ) -> Result<EvaluationResult, RoutingError> {
        self.check_deadline()?;
        let started = Instant::now();
        let mut candidates = Vec::new();
        for corridor in corridors {
            candidates.extend(self.evaluate_corridor(corridor).await?);
        }
        self.stats.candidates_before_pareto += candidates.len();
        let candidates = pareto_candidates(candidates, self.query.limits.max_candidates);
        self.stats.final_candidates = candidates.len();
        self.stats.bike_realizations_selected =
            candidates.iter().map(|c| c.metrics.bike_share_uses).sum();
        self.stats.phase2_latency_us +=
            u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        tracing::debug!(target:"via_server::routing",
            corridors_evaluated=self.stats.corridors_evaluated,
            corridor_states_evaluated=self.stats.corridor_states_evaluated,
            transit_departure_lookups=self.stats.transit_departure_lookups,
            walking_realizations_considered=self.stats.walking_realizations_considered,
            bike_lower_bound_checks=self.stats.bike_lower_bound_checks,
            bike_lower_bound_rejections=self.stats.bike_lower_bound_rejections,
            bike_exact_candidates_requested=self.stats.bike_exact_candidates_requested,
            bike_exact_cache_hits=self.stats.bike_exact_cache_hits,
            bike_matrix_requests=self.stats.bike_matrix_requests,
            bike_continuous_extensions=self.stats.bike_continuous_extensions,
            final_candidates=self.stats.final_candidates,
            phase2_latency_us=self.stats.phase2_latency_us,
            "Phase 2 corridor evaluation");
        Ok(EvaluationResult {
            variants: candidates.into_iter().map(|c| c.variant).collect(),
            stats: self.stats,
            failures: self.failures,
        })
    }

    fn check_deadline(&self) -> Result<(), RoutingError> {
        if self.query.deadline.is_some_and(|d| d.is_elapsed()) {
            Err(RoutingError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }
    fn charge(&mut self) -> Result<(), RoutingError> {
        self.check_deadline()?;
        if self.stats.corridor_states_evaluated >= self.query.limits.max_phase2_expansions {
            self.stats.expansion_limit_reached = true;
            return Err(RoutingError::Phase2EvaluationLimit);
        }
        self.stats.corridor_states_evaluated += 1;
        Ok(())
    }

    async fn evaluate_corridor(
        &mut self,
        corridor: &EvaluationCorridor,
    ) -> Result<Vec<EvaluatedCandidate>, RoutingError> {
        self.check_deadline()?;
        self.stats.corridors_evaluated += 1;
        if corridor
            .edges
            .iter()
            .filter(|edge| {
                edge.baseline
                    .iter()
                    .any(|b| matches!(b, BaselineMovement::Transit(_)))
            })
            .count()
            > self.query.limits.max_transfers.saturating_add(1)
        {
            self.stats.infeasible_transfer += 1;
            return Ok(Vec::new());
        }
        let mut states = vec![EvaluationState::initial(self.query)];
        let mut saw_missing_departure = false;
        for (index, edge) in corridor.edges.iter().enumerate() {
            let mut next = Vec::new();
            for state in states {
                self.check_deadline()?;
                let mut baseline = Vec::new();
                for movement in &edge.baseline {
                    match movement {
                        BaselineMovement::Transit(leg) => baseline.extend(self.expand_transit(
                            &state,
                            leg,
                            index,
                            &mut saw_missing_departure,
                        )?),
                        BaselineMovement::Walking {
                            kind,
                            duration_seconds,
                            distance_metres,
                            ready_at,
                        } => {
                            self.charge()?;
                            self.stats.surface_transfers_evaluated += 1;
                            let arrival = ready_at.unwrap_or(
                                state.ready_at + Duration::seconds(i64::from(*duration_seconds)),
                            );
                            if within_query_bounds(self.query, arrival) {
                                self.stats.walking_realizations_considered += 1;
                                baseline.push(state.after_movement(JourneyMovement {
                                    from_anchor: index,
                                    to_anchor: index + 1,
                                    departure_at: state.ready_at,
                                    arrival_at: arrival,
                                    kind: JourneyMovementKind::Walking {
                                        kind: *kind,
                                        duration_seconds: *duration_seconds,
                                        distance_metres: *distance_metres,
                                    },
                                    street_path: None,
                                }));
                            }
                        }
                    }
                }
                let best_baseline_arrival = baseline.iter().map(|s| s.ready_at).min();
                next.extend(baseline);
                for planner_index in 0..self.surface.len() {
                    let kind = self.surface[planner_index].kind();
                    if kind == SurfaceKind::Walking
                        && edge
                            .baseline
                            .iter()
                            .any(|b| matches!(b, BaselineMovement::Walking { .. }))
                    {
                        continue;
                    }
                    let (start_anchor, started_at) = if kind == SurfaceKind::BikeShare {
                        state.active_bike_run.as_ref().map_or_else(
                            || {
                                (
                                    index,
                                    state
                                        .bike_reuse_available_at
                                        .map_or(state.ready_at, |t| t.max(state.ready_at)),
                                )
                            },
                            |run| (run.start_anchor, run.started_at),
                        )
                    } else {
                        (index, state.ready_at)
                    };
                    let request = SurfaceRequest {
                        from: &corridor.anchors[start_anchor],
                        to: &corridor.anchors[index + 1],
                        departure_at: started_at,
                        from_anchor: start_anchor,
                        to_anchor: index + 1,
                    };
                    let Some(bound) = self.surface[planner_index].optimistic_duration(&request)
                    else {
                        continue;
                    };
                    if kind == SurfaceKind::BikeShare {
                        self.stats.bike_lower_bound_checks += 1;
                    }
                    let optimistic_arrival = started_at + Duration::seconds(i64::from(bound));
                    if best_baseline_arrival.is_some_and(|arrival| optimistic_arrival >= arrival)
                        || !within_query_bounds(self.query, optimistic_arrival)
                    {
                        if kind == SurfaceKind::BikeShare {
                            self.stats.bike_lower_bound_rejections += 1;
                        }
                        continue;
                    }
                    self.charge()?;
                    if kind == SurfaceKind::BikeShare {
                        self.stats.bike_exact_candidates_requested += 1;
                    }
                    let cache_hits = self.surface[planner_index].cache_hits();
                    let matrix_requests = self.surface[planner_index].matrix_requests();
                    let movements = self.surface[planner_index].realize(&request).await;
                    if kind == SurfaceKind::BikeShare {
                        let hits = self.surface[planner_index].cache_hits() - cache_hits;
                        self.stats.bike_exact_cache_hits += hits;
                        self.stats.bike_exact_candidates_requested -= hits;
                        self.stats.bike_matrix_requests +=
                            self.surface[planner_index].matrix_requests() - matrix_requests;
                    }
                    self.check_deadline()?;
                    let movements = match movements {
                        Ok(items) => items,
                        Err(error) => {
                            match error.failure_class() {
                                ProviderFailureClass::Transient => self.failures.transient = true,
                                ProviderFailureClass::Internal => self.failures.internal = true,
                                ProviderFailureClass::Unreachable => {}
                            }
                            continue;
                        }
                    };
                    for movement in movements {
                        match movement {
                            SurfaceMovement::Walking(path) => {
                                let arrival =
                                    state.ready_at + Duration::seconds(i64::from(path.duration.0));
                                if !within_query_bounds(self.query, arrival) {
                                    continue;
                                }
                                self.stats.walking_realizations_considered += 1;
                                let kind = match (
                                    &corridor.anchors[index].kind,
                                    &corridor.anchors[index + 1].kind,
                                ) {
                                    (JourneyAnchorKind::Origin, JourneyAnchorKind::Destination) => {
                                        WalkingMovementKind::Direct
                                    }
                                    (JourneyAnchorKind::Origin, _) => WalkingMovementKind::Access,
                                    (_, JourneyAnchorKind::Destination) => {
                                        WalkingMovementKind::Egress
                                    }
                                    _ => WalkingMovementKind::Transfer,
                                };
                                next.push(state.after_movement(JourneyMovement {
                                    from_anchor: index,
                                    to_anchor: index + 1,
                                    departure_at: state.ready_at,
                                    arrival_at: arrival,
                                    kind: JourneyMovementKind::Walking {
                                        kind,
                                        duration_seconds: path.duration.0,
                                        distance_metres: path.distance.0,
                                    },
                                    street_path: Some(path),
                                }));
                            }
                            SurfaceMovement::BikeShare(shortcut) => {
                                self.stats.bike_realizations_considered += 1;
                                let route_seconds = shortcut.total_seconds;
                                let arrival =
                                    started_at + Duration::seconds(i64::from(route_seconds));
                                if best_baseline_arrival.is_some_and(|baseline| arrival >= baseline)
                                    || !within_query_bounds(self.query, arrival)
                                {
                                    continue;
                                }
                                let wait = state.active_bike_run.as_ref().map_or_else(
                                    || {
                                        u32::try_from((started_at - state.ready_at).whole_seconds())
                                            .unwrap_or(u32::MAX)
                                    },
                                    |run| run.shortcut.rental_wait_seconds,
                                );
                                let Some(shortcut) = shortcut.with_rental_wait(wait) else {
                                    continue;
                                };
                                let prefix = state.active_bike_run.as_ref().map_or_else(
                                    || state.movements.clone(),
                                    |run| run.prefix_movements.clone(),
                                );
                                let departure = prefix
                                    .last()
                                    .map_or(self.query.departure_time, |m| m.arrival_at);
                                let mut movements = prefix.clone();
                                movements.push(JourneyMovement {
                                    from_anchor: start_anchor,
                                    to_anchor: index + 1,
                                    departure_at: departure,
                                    arrival_at: arrival,
                                    kind: JourneyMovementKind::BikeShare {
                                        shortcut: shortcut.clone(),
                                    },
                                    street_path: None,
                                });
                                if state.active_bike_run.is_some() {
                                    self.stats.bike_continuous_extensions += 1;
                                }
                                self.stats.bike_realizations_feasible += 1;
                                next.push(EvaluationState {
                                    ready_at: arrival,
                                    last_was_transit: false,
                                    active_bike_run: Some(ActiveBikeRun {
                                        start_anchor,
                                        started_at,
                                        prefix_movements: prefix,
                                        shortcut,
                                    }),
                                    bike_reuse_available_at: state.bike_reuse_available_at,
                                    movements,
                                });
                            }
                        }
                    }
                }
            }
            states = if index == 0 && !edge.baseline.is_empty() {
                next
            } else {
                prune_states(next, self.query.limits.max_candidates)
            };
            if states.is_empty() {
                break;
            }
        }
        let mut candidates = Vec::new();
        for state in states {
            let metrics = state.metrics();
            if metrics.transfer_count as usize > self.query.limits.max_transfers {
                continue;
            }
            let mut anchors = corridor
                .anchors
                .iter()
                .map(|a| JourneyAnchor {
                    kind: a.kind.clone(),
                    reached_at: self.query.departure_time,
                })
                .collect::<Vec<_>>();
            // Unvisited intermediate anchors of a continuous rental need monotonic times too.
            let mut reached_at = self.query.departure_time;
            for (index, anchor) in anchors.iter_mut().enumerate() {
                if let Some(movement) = state.movements.iter().find(|m| m.to_anchor == index) {
                    reached_at = movement.arrival_at;
                }
                anchor.reached_at = reached_at;
            }
            match JourneyVariant::from_movements(
                anchors,
                state.movements,
                self.query.departure_time,
            ) {
                Ok(variant) => candidates.push(EvaluatedCandidate { metrics, variant }),
                Err(_) => self.stats.malformed_trips += 1,
            }
        }
        if candidates.is_empty() {
            if saw_missing_departure {
                self.stats.infeasible_no_departure += 1;
            } else {
                self.stats.infeasible_constraints += 1;
            }
        } else {
            self.stats.corridors_feasible += 1;
        }
        Ok(candidates)
    }
}

fn within_query_bounds(query: &TransitQuery, time: OffsetDateTime) -> bool {
    time >= query.departure_time
        && time - query.departure_time
            <= Duration::seconds(i64::from(query.limits.max_journey_duration_seconds))
        && query.latest_arrival.is_none_or(|latest| time <= latest)
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
type TestBikeRoutes = std::collections::HashMap<CorridorMovement, Vec<BikeShortcut>>;

#[cfg(test)]
struct TestBikePlanner {
    routes: TestBikeRoutes,
    cache: std::collections::HashMap<(BoardingPointId, BoardingPointId), Vec<BikeShortcut>>,
    hits: usize,
}
#[cfg(test)]
impl TestBikePlanner {
    fn movement(&self, request: &SurfaceRequest<'_>) -> Option<CorridorMovement> {
        Some(CorridorMovement {
            from_anchor: request.from_anchor.checked_sub(1)?,
            to_anchor: request.to_anchor.checked_sub(1)?,
            from: request.from.boarding_point()?.clone(),
            to: request.to.boarding_point()?.clone(),
        })
    }
}
#[cfg(test)]
#[async_trait::async_trait]
impl SurfaceMovementPlanner for TestBikePlanner {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::BikeShare
    }
    fn cache_hits(&self) -> usize {
        self.hits
    }
    fn optimistic_duration(&self, request: &SurfaceRequest<'_>) -> Option<u32> {
        self.routes
            .get(&self.movement(request)?)
            .and_then(|items| items.iter().map(|s| s.total_seconds / 2).min())
    }
    async fn realize(
        &mut self,
        request: &SurfaceRequest<'_>,
    ) -> Result<Vec<SurfaceMovement>, crate::street::StreetRoutingError> {
        let Some(movement) = self.movement(request) else {
            return Ok(Vec::new());
        };
        let key = (movement.from.clone(), movement.to.clone());
        let mut options = if let Some(cached) = self.cache.get(&key) {
            self.hits += 1;
            cached.clone()
        } else {
            let options = self.routes.get(&movement).cloned().unwrap_or_default();
            self.cache.insert(key, options.clone());
            options
        };
        for shortcut in &mut options {
            shortcut.from_anchor = request.from_anchor;
            shortcut.to_anchor = request.to_anchor;
        }
        Ok(options
            .into_iter()
            .map(SurfaceMovement::BikeShare)
            .collect())
    }
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
    routes: &TestBikeRoutes,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    run_test_evaluation(network, corridor, snapshot, query, Some(routes))
}
#[cfg(test)]
fn run_test_evaluation(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
    routes: Option<&TestBikeRoutes>,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(async {
            if query.origin_access.is_empty() || query.destination_egress.is_empty() {
                return Err(RoutingError::InvalidQuery);
            }
            let evaluation_corridors = corridor
                .retained()
                .iter()
                .filter_map(|retained| EvaluationCorridor::from_transit(network, retained, &query))
                .collect::<Vec<_>>();
            let mut surface: Vec<Box<dyn SurfaceMovementPlanner>> = routes
                .map(|routes| {
                    vec![Box::new(TestBikePlanner {
                        routes: routes.clone(),
                        cache: std::collections::HashMap::new(),
                        hits: 0,
                    }) as Box<dyn SurfaceMovementPlanner>]
                })
                .unwrap_or_default();
            let result =
                FixedCorridorEvaluator::new(network, corridor, snapshot, &query, &mut surface)
                    .evaluate(&evaluation_corridors)
                    .await?;
            Ok((result.variants, result.stats))
        })
}
