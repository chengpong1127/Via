mod candidate;
mod expansion;
mod pareto;
mod state;
#[cfg(test)]
mod tests;
pub(crate) use expansion::WalkingCache;

#[cfg(test)]
use super::bike::{BikeMovementRequest, BikeSessionStats, TestBikePlanner as TestBikeProvider};
use super::{
    bike::BikeShareSession,
    corridor::{CorridorEdge, EvaluationCorridor, PlannedMovement},
    model::{PlanningQuery, RoutingError},
    realized::{
        BikeShareMovement, JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
        RealizedJourney, WalkingMovementKind,
    },
    static_search::{RetainedCorridorSet, StaticTransitLeg},
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
    pub bike_exact_candidates_requested: usize,
    pub bike_exact_cache_hits: usize,
    pub bike_matrix_requests: usize,
    pub walking_matrix_requests: usize,
    pub walking_cache_hits: usize,
    pub expansion_limit_reached: bool,
    pub corridors_completed: usize,
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
    walking: &'a mut WalkingCache,
    bike_share: Option<&'a mut BikeShareSession>,
    pub(super) stats: Phase2EvaluationStats,
    pub(super) failures: EvaluationFailures,
}

impl<'a> FixedCorridorEvaluator<'a> {
    pub fn new(
        network: &'a TransitRuntime,
        corridor: &'a RetainedCorridorSet,
        snapshot: &'a TimingSnapshot,
        query: &'a PlanningQuery,
        walking: &'a mut WalkingCache,
        bike_share: Option<&'a mut BikeShareSession>,
    ) -> Self {
        Self {
            network,
            corridor,
            snapshot,
            query,
            walking,
            bike_share,
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
            match self.evaluate_corridor(corridor).await {
                Ok(completed) => {
                    self.stats.corridors_completed += 1;
                    candidates.extend(completed);
                }
                Err(RoutingError::Phase2EvaluationLimit) => {
                    self.stats.expansion_limit_reached = true;
                    ::metrics::counter!("via_planner_phase2_budget_exhaustions_total").increment(1);
                    tracing::warn!(
                        target: "via_server::routing",
                        states_evaluated = self.stats.corridor_states_evaluated,
                        corridors_completed = self.stats.corridors_completed,
                        completed_candidates = candidates.len(),
                        "Phase 2 budget exhausted; retaining only completed corridors"
                    );
                    break;
                }
                Err(error) => return Err(error),
            }
        }

        self.check_deadline()?;
        let candidates = pareto_candidates(candidates);
        self.check_deadline()?;
        self.stats.final_candidates = candidates.len();
        self.stats.phase2_latency_us +=
            u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let bike_work = self
            .bike_share
            .as_ref()
            .map(|planner| planner.realization_stats())
            .unwrap_or_default();
        self.stats.walking_matrix_requests = self.walking.requests;
        self.stats.walking_cache_hits = self.walking.cache_hits;
        self.stats.bike_exact_candidates_requested = bike_work.exact_requests;
        self.stats.bike_exact_cache_hits = bike_work.cache_hits;
        self.stats.bike_matrix_requests = bike_work.matrix_requests;
        tracing::debug!(
            target: "via_server::routing",
            states_evaluated = self.stats.corridor_states_evaluated,
            departure_lookups = self.stats.transit_departure_lookups,
            bike_exact_requests = self.stats.bike_exact_candidates_requested,
            bike_cache_hits = self.stats.bike_exact_cache_hits,
            bike_matrix_requests = self.stats.bike_matrix_requests,
            final_candidates = self.stats.final_candidates,
            expansion_limit_reached = self.stats.expansion_limit_reached,
            corridors_completed = self.stats.corridors_completed,
            walking_matrix_requests = self.stats.walking_matrix_requests,
            walking_cache_hits = self.stats.walking_cache_hits,
            latency_us = self.stats.phase2_latency_us,
            "Phase 2 corridor evaluation"
        );
        if self.stats.expansion_limit_reached && candidates.is_empty() {
            return Err(RoutingError::Phase2EvaluationLimit);
        }
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
    fn movement(&self, request: &BikeMovementRequest<'_>) -> Option<CorridorMovement> {
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
impl TestBikeProvider for TestBikePlanner {
    fn realization_stats(&self) -> BikeSessionStats {
        BikeSessionStats {
            exact_requests: self.cache.len(),
            cache_hits: self.hits,
            matrix_requests: 0,
        }
    }
    fn has_candidate(&self, request: &BikeMovementRequest<'_>) -> bool {
        self.movement(request)
            .and_then(|movement| self.routes.get(&movement))
            .is_some_and(|items| !items.is_empty())
    }
    async fn realize(
        &mut self,
        request: &BikeMovementRequest<'_>,
    ) -> Result<Vec<BikeShareMovement>, crate::street::StreetRoutingError> {
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
        Ok(options)
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
            let mut dependencies = test_dependencies();
            dependencies.bike_share = routes.map(|routes| {
                BikeShareSession::for_test(Box::new(TestBikePlanner {
                    routes: routes.clone(),
                    cache: std::collections::HashMap::new(),
                    hits: 0,
                }))
            });
            let result = FixedCorridorEvaluator::new(
                network,
                corridor,
                snapshot,
                &query,
                &mut dependencies.walking,
                dependencies.bike_share.as_mut(),
            )
            .evaluate(&evaluation_corridors)
            .await?;
            Ok((result.journeys, result.stats))
        })
}

#[cfg(test)]
struct TestDependencies {
    walking: WalkingCache,
    bike_share: Option<BikeShareSession>,
}

#[cfg(test)]
fn test_dependencies() -> TestDependencies {
    TestDependencies {
        walking: WalkingCache::new(Arc::new(crate::street::FakeStreetRouter::builder().build())),
        bike_share: None,
    }
}
