mod candidate;
mod expansion;
mod pareto;
#[cfg(test)]
#[path = "../../../tests/support/pareto_reference.rs"]
pub(crate) mod pareto_reference;
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
    pub states_expanded: usize,
    pub transit_operations_attempted: usize,
    pub walking_operations_attempted: usize,
    pub bike_operations_attempted: usize,
    pub successors_generated: usize,
    pub corridors_attempted: usize,
    pub transit_departure_lookups: usize,
    pub bike_exact_candidates_requested: usize,
    pub bike_exact_cache_hits: usize,
    pub bike_matrix_requests: usize,
    pub walking_matrix_requests: usize,
    pub walking_cache_hits: usize,
    pub corridors_completed: usize,
    pub final_candidates: usize,
    pub phase2_latency_us: u64,
    pub matrix: crate::routing::MatrixResolverStats,
}

impl Phase2EvaluationStats {
    fn merge_corridor(&mut self, stats: Self) {
        self.states_expanded += stats.states_expanded;
        self.transit_operations_attempted += stats.transit_operations_attempted;
        self.walking_operations_attempted += stats.walking_operations_attempted;
        self.bike_operations_attempted += stats.bike_operations_attempted;
        self.successors_generated += stats.successors_generated;
        self.transit_departure_lookups += stats.transit_departure_lookups;
    }

    fn record_expansion(&mut self, stats: expansion::ExpansionStats) {
        self.states_expanded += stats.states_expanded;
        self.transit_operations_attempted += stats.transit_operations_attempted;
        self.transit_departure_lookups += stats.transit_departure_lookups;
        self.walking_operations_attempted += stats.walking_operations_attempted;
        self.bike_operations_attempted += stats.bike_operations_attempted;
        self.successors_generated += stats.successors_generated;
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct EvaluationFailures {
    pub transient: bool,
    pub internal: bool,
}

impl EvaluationFailures {
    fn merge(&mut self, other: Self) {
        self.transient |= other.transient;
        self.internal |= other.internal;
    }
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
    walking: &'a WalkingCache,
    bike_share: Option<&'a BikeShareSession>,
    #[cfg(test)]
    state_pruner: StatePruner,
    pub(super) stats: Phase2EvaluationStats,
    pub(super) failures: EvaluationFailures,
}

#[cfg(test)]
type StatePruner = fn(Vec<EvaluationState>, usize) -> Vec<EvaluationState>;

impl<'a> FixedCorridorEvaluator<'a> {
    pub fn new(
        network: &'a TransitRuntime,
        corridor: &'a RetainedCorridorSet,
        snapshot: &'a TimingSnapshot,
        query: &'a PlanningQuery,
        walking: &'a WalkingCache,
        bike_share: Option<&'a BikeShareSession>,
    ) -> Self {
        Self {
            network,
            corridor,
            snapshot,
            query,
            walking,
            bike_share,
            #[cfg(test)]
            state_pruner: prune_states,
            stats: Phase2EvaluationStats::default(),
            failures: EvaluationFailures::default(),
        }
    }
    #[tracing::instrument(target = "via_profile", name = "Corridor Evaluation", skip_all, fields(corridor_count = corridors.len(), states_expanded = tracing::field::Empty, candidate_count = tracing::field::Empty, walking_cache_hits = tracing::field::Empty, bike_cache_hits = tracing::field::Empty))]
    pub async fn evaluate(
        &mut self,
        corridors: &[EvaluationCorridor],
    ) -> Result<EvaluationResult, RoutingError> {
        let started = Instant::now();
        let result = self.evaluate_inner(corridors).await;
        self.stats.phase2_latency_us +=
            u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.stats.matrix = self.walking.matrix_stats();
        self.stats.walking_matrix_requests = self.walking.requests();
        self.stats.walking_cache_hits = self.walking.cache_hits();
        let bike = self
            .bike_share
            .map(BikeShareSession::realization_stats)
            .unwrap_or_default();
        self.stats.bike_exact_candidates_requested = bike.exact_requests;
        self.stats.bike_exact_cache_hits = bike.cache_hits;
        self.stats.bike_matrix_requests = bike.matrix_requests;
        tracing::Span::current()
            .record("states_expanded", self.stats.states_expanded)
            .record("candidate_count", self.stats.final_candidates)
            .record("walking_cache_hits", self.stats.walking_cache_hits)
            .record("bike_cache_hits", self.stats.bike_exact_cache_hits);
        tracing::debug!(
            target: "via_server::routing",
            states_expanded = self.stats.states_expanded,
            transit_operations_attempted = self.stats.transit_operations_attempted,
            walking_operations_attempted = self.stats.walking_operations_attempted,
            bike_operations_attempted = self.stats.bike_operations_attempted,
            successors_generated = self.stats.successors_generated,
            corridors_attempted = self.stats.corridors_attempted,
            departure_lookups = self.stats.transit_departure_lookups,
            bike_exact_requests = self.stats.bike_exact_candidates_requested,
            bike_cache_hits = self.stats.bike_exact_cache_hits,
            bike_matrix_requests = self.stats.bike_matrix_requests,
            final_candidates = self.stats.final_candidates,
            corridors_completed = self.stats.corridors_completed,
            walking_matrix_requests = self.stats.walking_matrix_requests,
            walking_cache_hits = self.stats.walking_cache_hits,
            latency_us = self.stats.phase2_latency_us,
            "Phase 2 corridor evaluation"
        );
        result.map(|mut result| {
            result.stats = self.stats;
            result
        })
    }

    async fn evaluate_inner(
        &mut self,
        corridors: &[EvaluationCorridor],
    ) -> Result<EvaluationResult, RoutingError> {
        self.check_deadline()?;
        use futures_util::{StreamExt, TryStreamExt, stream};
        use tracing::Instrument;
        let concurrency = self
            .query
            .limits
            .max_concurrent_phase2_corridors
            .clamp(1, 8);
        let network = self.network;
        let retained = self.corridor;
        let snapshot = self.snapshot;
        let query = self.query;
        let walking = self.walking;
        let bike_share = self.bike_share;
        #[cfg(test)]
        let state_pruner = self.state_pruner;
        let mut attempted = 0;
        let results = stream::iter(0..corridors.len())
            .map(|index| {
                attempted += 1;
                async move {
                    let mut evaluator = FixedCorridorEvaluator::new(
                        network, retained, snapshot, query, walking, bike_share,
                    );
                    #[cfg(test)]
                    {
                        evaluator.state_pruner = state_pruner;
                    }
                    let span = tracing::info_span!(target: "via_profile", "Phase 2 Corridor", corridor_index = index);
                    let result = evaluator.evaluate_corridor(&corridors[index]).instrument(span).await;
                    (index, result, evaluator.stats, evaluator.failures)
                }
            })
            .buffer_unordered(concurrency)
            .map(|(index, result, stats, failures)| {
                self.stats.merge_corridor(stats);
                self.failures.merge(failures);
                if result.is_ok() {
                    self.stats.corridors_completed += 1;
                }
                result.map(|candidates| (index, candidates))
            });
        let results = query.deadline.run(results.try_collect::<Vec<_>>()).await;
        self.stats.corridors_attempted += attempted;
        let mut results = results??;
        results.sort_unstable_by_key(|(index, _)| *index);
        let candidates: Vec<_> = results
            .into_iter()
            .flat_map(|(_, candidates)| candidates)
            .collect();
        self.check_deadline()?;
        let candidates = pareto_candidates(candidates);
        self.check_deadline()?;
        self.stats.final_candidates = candidates.len();
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
        let resolver = expansion::ExpansionResolver {
            inputs: expansion::ExpansionInputs {
                network: self.network,
                corridor: self.corridor,
                snapshot: self.snapshot,
                query: self.query,
            },
            walking: self.walking,
            bike_share: self.bike_share,
        };
        let mut states = vec![EvaluationState::initial(self.query)];
        for (index, edge) in corridor.edges().iter().enumerate() {
            let outcome = resolver.expand_edge(&states, corridor, edge, index).await;
            self.stats.record_expansion(outcome.stats);
            self.failures.merge(outcome.failures);
            let successors = outcome.successors?;
            #[cfg(test)]
            let next = (self.state_pruner)(successors, self.query.limits.max_candidates);
            #[cfg(not(test))]
            let next = prune_states(successors, self.query.limits.max_candidates);
            states = next;
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
    cache: std::sync::Mutex<
        std::collections::HashMap<(BoardingPointId, BoardingPointId), Vec<BikeShareMovement>>,
    >,
    hits: std::sync::atomic::AtomicUsize,
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
            exact_requests: self.cache.lock().unwrap().len(),
            cache_hits: self.hits.load(std::sync::atomic::Ordering::Relaxed),
            matrix_requests: 0,
        }
    }
    fn has_candidate(&self, request: &BikeMovementRequest<'_>) -> bool {
        self.movement(request)
            .and_then(|movement| self.routes.get(&movement))
            .is_some_and(|items| !items.is_empty())
    }
    async fn realize(
        &self,
        request: &BikeMovementRequest<'_>,
    ) -> Result<Vec<BikeShareMovement>, crate::street::StreetRoutingError> {
        let Some(movement) = self.movement(request) else {
            return Ok(Vec::new());
        };
        let key = (movement.from.clone(), movement.to.clone());
        let mut cache = self.cache.lock().unwrap();
        let mut options = if let Some(cached) = cache.get(&key) {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            cached.clone()
        } else {
            let options = self.routes.get(&movement).cloned().unwrap_or_default();
            cache.insert(key, options.clone());
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
    run_test_evaluation(
        network,
        corridor,
        snapshot,
        query,
        None,
        pareto::prune_states,
    )
}
#[cfg(test)]
fn evaluate_with_test_bike_routes(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
    routes: &TestBikeRoutes,
) -> Result<(Vec<RealizedJourney>, Phase2EvaluationStats), RoutingError> {
    run_test_evaluation(
        network,
        corridor,
        snapshot,
        query,
        Some(routes),
        pareto::prune_states,
    )
}
#[cfg(test)]
fn evaluate_with_reference_state_pruning(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
) -> Result<Vec<RealizedJourney>, RoutingError> {
    run_test_evaluation(
        network,
        corridor,
        snapshot,
        query,
        None,
        pareto::prune_states_reference,
    )
    .map(|(journeys, _)| journeys)
}
#[cfg(test)]
fn run_test_evaluation(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    snapshot: &TimingSnapshot,
    query: PlanningQuery,
    routes: Option<&TestBikeRoutes>,
    state_pruner: StatePruner,
) -> Result<(Vec<RealizedJourney>, Phase2EvaluationStats), RoutingError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
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
                    cache: Default::default(),
                    hits: Default::default(),
                }))
            });
            let mut evaluator = FixedCorridorEvaluator::new(
                network,
                corridor,
                snapshot,
                &query,
                &dependencies.walking,
                dependencies.bike_share.as_ref(),
            );
            evaluator.state_pruner = state_pruner;
            let result = evaluator.evaluate(&evaluation_corridors).await?;
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
