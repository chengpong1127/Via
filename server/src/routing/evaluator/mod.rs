mod bike;
mod candidate;
mod expansion;
mod pareto;
mod state;
#[cfg(test)]
mod tests;
mod transit;
mod walking;

use super::{
    corridor::{CorridorEdge, EvaluationCorridor, PlannedMovement},
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

        let candidates = pareto_candidates(candidates);
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
    async fn evaluate_corridor(
        &mut self,
        corridor: &EvaluationCorridor,
    ) -> Result<Vec<EvaluatedCandidate>, RoutingError> {
        self.check_deadline()?;
        if !self.corridor_within_transfer_limit(corridor) {
            return Ok(Vec::new());
        }
        let mut states = vec![EvaluationState::initial(self.query)];
        for (index, edge) in corridor.edges().iter().enumerate() {
            let mut next = Vec::new();
            for state in states {
                let successors = self.expand_state(&state, corridor, edge, index).await?;
                next.extend(successors);
            }
            states = prune_states(next, self.query.limits.max_candidates);
            if states.is_empty() {
                break;
            }
        }
        self.finalize_states(corridor, states)
    }

    fn corridor_within_transfer_limit(&self, corridor: &EvaluationCorridor) -> bool {
        corridor
            .edges()
            .iter()
            .filter(|edge| matches!(edge.planned_movement, PlannedMovement::Transit(_)))
            .count()
            <= self.query.limits.max_transfers.saturating_add(1)
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
