mod bike;
mod pareto;
mod state;
#[cfg(test)]
mod tests;
mod transit;

use super::{
    corridor::{BaselineMovement, EvaluationCorridor},
    model::{PlanningQuery, RoutingError},
    realized::{
        BikeShareMovement, JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
        RealizedJourney, WalkingMovementKind,
    },
    static_search::{RetainedCorridorSet, StaticTransitLeg},
    surface::{
        SurfaceKind, SurfaceMovement, SurfaceMovementPlanner, SurfaceRealizationStats,
        SurfaceRequest,
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
use state::{EvaluationState, PathMetrics};
use std::{sync::Arc, time::Instant};
use time::{Duration, OffsetDateTime};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phase2EvaluationStats {
    pub corridor_states_evaluated: usize,
    pub transit_departure_lookups: usize,
    pub bike_lower_bound_checks: usize,
    pub bike_lower_bound_rejections: usize,
    pub bike_exact_candidates_requested: usize,
    pub bike_exact_cache_hits: usize,
    pub bike_matrix_requests: usize,
    pub final_candidates: usize,
    pub phase2_latency_us: u64,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct EvaluationFailures {
    pub transient: bool,
    pub internal: bool,
}

pub(super) struct EvaluationResult {
    pub journeys: Vec<RealizedJourney>,
    pub stats: Phase2EvaluationStats,
    pub failures: EvaluationFailures,
}

/// One request-local evaluator is retained across transit evaluation and fallback,
/// preserving movement caches and the total expansion budget.
pub(super) struct FixedCorridorEvaluator<'a> {
    network: &'a TransitRuntime,
    corridor: &'a RetainedCorridorSet,
    snapshot: &'a TimingSnapshot,
    query: &'a PlanningQuery,
    surface_planners: &'a mut [Box<dyn SurfaceMovementPlanner>],
    pub(super) stats: Phase2EvaluationStats,
    pub(super) failures: EvaluationFailures,
}

impl<'a> FixedCorridorEvaluator<'a> {
    pub fn new(
        network: &'a TransitRuntime,
        corridor: &'a RetainedCorridorSet,
        snapshot: &'a TimingSnapshot,
        query: &'a PlanningQuery,
        surface_planners: &'a mut [Box<dyn SurfaceMovementPlanner>],
    ) -> Self {
        Self {
            network,
            corridor,
            snapshot,
            query,
            surface_planners,
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

        let candidates = pareto_candidates(candidates, self.query.limits.max_candidates);
        self.stats.final_candidates = candidates.len();
        self.stats.phase2_latency_us +=
            u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let bike_work = self
            .surface_planners
            .iter()
            .filter(|planner| planner.kind() == SurfaceKind::BikeShare)
            .map(|planner| planner.realization_stats())
            .fold(SurfaceRealizationStats::default(), |mut total, stats| {
                total.exact_requests += stats.exact_requests;
                total.cache_hits += stats.cache_hits;
                total.matrix_requests += stats.matrix_requests;
                total
            });
        self.stats.bike_exact_candidates_requested = bike_work.exact_requests;
        self.stats.bike_exact_cache_hits = bike_work.cache_hits;
        self.stats.bike_matrix_requests = bike_work.matrix_requests;
        tracing::debug!(
            target: "via_server::routing",
            states_evaluated = self.stats.corridor_states_evaluated,
            departure_lookups = self.stats.transit_departure_lookups,
            bike_lower_bound_checks = self.stats.bike_lower_bound_checks,
            bike_lower_bound_rejections = self.stats.bike_lower_bound_rejections,
            bike_exact_requests = self.stats.bike_exact_candidates_requested,
            bike_cache_hits = self.stats.bike_exact_cache_hits,
            bike_matrix_requests = self.stats.bike_matrix_requests,
            final_candidates = self.stats.final_candidates,
            latency_us = self.stats.phase2_latency_us,
            "Phase 2 corridor evaluation"
        );
        Ok(EvaluationResult {
            journeys: candidates.into_iter().map(|c| c.journey).collect(),
            stats: self.stats,
            failures: self.failures,
        })
    }

    fn check_deadline(&self) -> Result<(), RoutingError> {
        if self.query.deadline.is_elapsed() {
            Err(RoutingError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }
    fn charge(&mut self) -> Result<(), RoutingError> {
        self.check_deadline()?;
        if self.stats.corridor_states_evaluated >= self.query.limits.max_phase2_expansions {
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
        if corridor
            .edges()
            .iter()
            .filter(|edge| {
                edge.baseline
                    .iter()
                    .any(|b| matches!(b, BaselineMovement::Transit(_)))
            })
            .count()
            > self.query.limits.max_transfers.saturating_add(1)
        {
            return Ok(Vec::new());
        }
        let mut states = vec![EvaluationState::initial(self.query)];
        for (index, edge) in corridor.edges().iter().enumerate() {
            let mut next = Vec::new();
            for state in states {
                self.check_deadline()?;
                let mut baseline = Vec::new();
                for movement in &edge.baseline {
                    match movement {
                        BaselineMovement::Transit(leg) => {
                            baseline.extend(self.expand_transit(&state, leg, index)?)
                        }
                        BaselineMovement::RetainedWalking {
                            kind,
                            duration_seconds,
                            distance_metres,
                            access_available_at,
                        } => {
                            self.charge()?;
                            let arrival = access_available_at.unwrap_or(
                                state.ready_at + Duration::seconds(i64::from(*duration_seconds)),
                            );
                            if within_query_bounds(self.query, arrival) {
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
                for planner_index in 0..self.surface_planners.len() {
                    let kind = self.surface_planners[planner_index].kind();
                    let (start_anchor, started_at) = if kind == SurfaceKind::BikeShare {
                        state.bike_departure(index)
                    } else {
                        (index, state.ready_at)
                    };
                    let request = SurfaceRequest {
                        retained_walking_baseline: edge
                            .baseline
                            .iter()
                            .any(|b| matches!(b, BaselineMovement::RetainedWalking { .. })),
                        from: &corridor.anchors()[start_anchor],
                        to: &corridor.anchors()[index + 1],
                        departure_at: started_at,
                        from_anchor: start_anchor,
                        to_anchor: index + 1,
                    };
                    let Some(bound) =
                        self.surface_planners[planner_index].optimistic_duration(&request)
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
                    let movements = self.surface_planners[planner_index].realize(&request).await;
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

                                let kind = match (
                                    &corridor.anchors()[index].kind,
                                    &corridor.anchors()[index + 1].kind,
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
                            SurfaceMovement::BikeShare(bike_movement) => {
                                let route_seconds = bike_movement.total_seconds;
                                let arrival =
                                    started_at + Duration::seconds(i64::from(route_seconds));
                                if best_baseline_arrival.is_some_and(|baseline| arrival >= baseline)
                                    || !within_query_bounds(self.query, arrival)
                                {
                                    continue;
                                }
                                if let Some(next_state) = state.after_bike_share(
                                    bike_movement,
                                    start_anchor,
                                    index + 1,
                                    started_at,
                                    arrival,
                                    self.query.departure_time,
                                ) {
                                    next.push(next_state);
                                }
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
                .anchors()
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
            match RealizedJourney::from_movements(
                anchors,
                state.movements,
                self.query.departure_time,
            ) {
                Ok(journey) => candidates.push(EvaluatedCandidate { metrics, journey }),
                Err(error) => {
                    return Err(RoutingError::Internal(format!(
                        "evaluated journey has invalid movement continuity: {error}"
                    )));
                }
            }
        }
        Ok(candidates)
    }
}

fn within_query_bounds(query: &PlanningQuery, time: OffsetDateTime) -> bool {
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
type TestBikeRoutes = std::collections::HashMap<CorridorMovement, Vec<BikeShareMovement>>;

#[cfg(test)]
struct TestBikePlanner {
    routes: TestBikeRoutes,
    cache: std::collections::HashMap<(BoardingPointId, BoardingPointId), Vec<BikeShareMovement>>,
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
    fn realization_stats(&self) -> SurfaceRealizationStats {
        SurfaceRealizationStats {
            exact_requests: self.cache.len(),
            cache_hits: self.hits,
            matrix_requests: 0,
        }
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
        for bike_movement in &mut options {
            bike_movement.from_anchor = request.from_anchor;
            bike_movement.to_anchor = request.to_anchor;
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
    query: PlanningQuery,
) -> Result<Vec<RealizedJourney>, RoutingError> {
    evaluate_with_stats(network, corridor, snapshot, query).map(|(items, _)| items)
}
#[cfg(test)]
fn evaluate_with_stats(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
) -> Result<(Vec<RealizedJourney>, Phase2EvaluationStats), RoutingError> {
    run_test_evaluation(network, corridor, snapshot, query, None)
}
#[cfg(test)]
fn evaluate_with_test_bike_routes(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
    routes: &TestBikeRoutes,
) -> Result<(Vec<RealizedJourney>, Phase2EvaluationStats), RoutingError> {
    run_test_evaluation(network, corridor, snapshot, query, Some(routes))
}
#[cfg(test)]
fn run_test_evaluation(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
    routes: Option<&TestBikeRoutes>,
) -> Result<(Vec<RealizedJourney>, Phase2EvaluationStats), RoutingError> {
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
                .map(|retained| EvaluationCorridor::from_transit(network, retained, &query))
                .collect::<Result<Vec<_>, _>>()?;
            let mut surface_planners: Vec<Box<dyn SurfaceMovementPlanner>> = routes
                .map(|routes| {
                    vec![Box::new(TestBikePlanner {
                        routes: routes.clone(),
                        cache: std::collections::HashMap::new(),
                        hits: 0,
                    }) as Box<dyn SurfaceMovementPlanner>]
                })
                .unwrap_or_default();
            let result = FixedCorridorEvaluator::new(
                network,
                corridor,
                snapshot,
                &query,
                &mut surface_planners,
            )
            .evaluate(&evaluation_corridors)
            .await?;
            Ok((result.journeys, result.stats))
        })
}
