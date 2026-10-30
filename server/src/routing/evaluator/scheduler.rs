use super::*;

pub(in crate::routing) struct CorridorScheduler<'a> {
    network: &'a TransitRuntime,
    corridor: &'a RetainedCorridorSet,
    snapshot: &'a TimingSnapshot,
    query: &'a PlanningQuery,
    walking: &'a WalkingCache,
    bike_share: Option<&'a BikeShareSession>,
    #[cfg(test)]
    pub(super) state_pruner: StatePruner,
}

impl<'a> CorridorScheduler<'a> {
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
        }
    }
    #[tracing::instrument(target = "via_profile", name = "Corridor Evaluation", skip_all, fields(corridor_count = corridors.len(), states_expanded = tracing::field::Empty, candidate_count = tracing::field::Empty, walking_cache_hits = tracing::field::Empty, bike_cache_hits = tracing::field::Empty))]
    pub async fn evaluate(self, corridors: &[EvaluationCorridor]) -> EvaluationReport {
        let started = Instant::now();
        let mut stats = Phase2EvaluationStats::default();
        let mut failures = FailureSummary::default();
        let outcome = self
            .evaluate_inner(corridors, &mut stats, &mut failures)
            .await;
        stats.phase2_latency_us += u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        stats.matrix = self.walking.matrix_stats();
        stats.walking_matrix_requests = self.walking.requests();
        stats.walking_cache_hits = self.walking.cache_hits();
        let bike = self
            .bike_share
            .map(BikeShareSession::realization_stats)
            .unwrap_or_default();
        stats.bike_exact_candidates_requested = bike.exact_requests;
        stats.bike_exact_cache_hits = bike.cache_hits;
        stats.bike_matrix_requests = bike.matrix_requests;
        tracing::Span::current()
            .record("states_expanded", stats.states_expanded)
            .record("candidate_count", stats.final_candidates)
            .record("walking_cache_hits", stats.walking_cache_hits)
            .record("bike_cache_hits", stats.bike_exact_cache_hits);
        tracing::debug!(
            target: "via_server::routing",
            states_expanded = stats.states_expanded,
            transit_operations_attempted = stats.transit_operations_attempted,
            walking_operations_attempted = stats.walking_operations_attempted,
            bike_operations_attempted = stats.bike_operations_attempted,
            successors_generated = stats.successors_generated,
            corridors_attempted = stats.corridors_attempted,
            departure_lookups = stats.transit_departure_lookups,
            bike_exact_requests = stats.bike_exact_candidates_requested,
            bike_cache_hits = stats.bike_exact_cache_hits,
            bike_matrix_requests = stats.bike_matrix_requests,
            final_candidates = stats.final_candidates,
            corridors_completed = stats.corridors_completed,
            walking_matrix_requests = stats.walking_matrix_requests,
            walking_cache_hits = stats.walking_cache_hits,
            latency_us = stats.phase2_latency_us,
            "Phase 2 corridor evaluation"
        );
        EvaluationReport {
            outcome,
            stats,
            failures,
        }
    }

    async fn evaluate_inner(
        &self,
        corridors: &[EvaluationCorridor],
        stats: &mut Phase2EvaluationStats,
        failures: &mut FailureSummary,
    ) -> Result<Vec<RealizedJourney>, RoutingError> {
        check_deadline(self.query)?;
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
            .map(|(index, result, corridor_stats, corridor_failures)| {
                stats.merge_corridor(corridor_stats);
                failures.merge(corridor_failures);
                if result.is_ok() {
                    stats.corridors_completed += 1;
                }
                result.map(|candidates| (index, candidates))
            });
        let results = query.deadline.run(results.try_collect::<Vec<_>>()).await;
        stats.corridors_attempted += attempted;
        let mut results = results??;
        results.sort_unstable_by_key(|(index, _)| *index);
        let candidates: Vec<_> = results
            .into_iter()
            .flat_map(|(_, candidates)| candidates)
            .collect();
        check_deadline(self.query)?;
        let candidates = pareto_candidates(candidates);
        check_deadline(self.query)?;
        stats.final_candidates = candidates.len();
        Ok(candidates.into_iter().map(|c| c.journey).collect())
    }
}
