use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use time::OffsetDateTime;

use crate::{
    domain::{
        bikeshare::{BikeAvailabilitySource, BikeShareNetwork},
        journey::{EmptyReason, EmptyResult, OptionsResult, PlanningError, PlanningOutcome},
        planning::PlanningInput,
        ranking::RankingPolicy,
        transit_runtime::TransitRuntime,
        transit_timing::TimingSnapshotLoader,
    },
    street::StreetRouter,
};

use super::{
    model::{
        FailureSummary, JourneyPlanningContext, JourneyPlanningLimits, PlanningDeadline,
        PlanningQuery, RoutingError,
    },
    multimodal::MultimodalPlanner,
    realized::RealizedJourney,
    reconstruct::{
        JourneyCandidate, JourneyReconstructor, apply_ranked_journey, assign_result_local_ids,
        taipei_now,
    },
};

#[derive(Clone)]
pub struct RoutingService {
    planner: MultimodalPlanner,
    reconstructor: JourneyReconstructor,
    ranking: RankingPolicy,
    limits: JourneyPlanningLimits,
    request_timeout: Option<std::time::Duration>,
    request_slots: Arc<tokio::sync::Semaphore>,
}

impl RoutingService {
    pub fn new(
        street_routing: Arc<dyn StreetRouter>,
        transit_runtime: Arc<TransitRuntime>,
        timing: Arc<dyn TimingSnapshotLoader>,
        bike_share_network: Arc<BikeShareNetwork>,
        bike_availability_source: Arc<dyn BikeAvailabilitySource>,
        limits: JourneyPlanningLimits,
        request_timeouts_enabled: bool,
    ) -> Self {
        let planner = MultimodalPlanner::new(
            transit_runtime.clone(),
            timing,
            street_routing.clone(),
            bike_share_network.clone(),
            bike_availability_source,
            limits.bike.clone(),
        );
        let reconstructor =
            JourneyReconstructor::new(street_routing.clone(), transit_runtime, bike_share_network);
        let limits = limits.normalized();
        Self {
            planner,
            reconstructor,
            ranking: RankingPolicy {
                result_limit: limits
                    .ranking
                    .result_limit
                    .min(limits.planner.max_candidates),
                ..limits.ranking.clone()
            },
            request_slots: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_requests)),
            request_timeout: request_timeouts_enabled.then_some(limits.request_timeout),
            limits,
        }
    }
}

impl RoutingService {
    #[tracing::instrument(target = "via_profile", name = "Routing Request", skip_all)]
    pub async fn plan(&self, request: PlanningInput) -> Result<PlanningOutcome, PlanningError> {
        let deadline = self
            .request_timeout
            .map_or_else(PlanningDeadline::disabled, PlanningDeadline::after);
        match deadline
            .run(async {
                let _permit = match self.request_slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!(
                            target: "via_server::routing",
                            "journey planning rejected because routing capacity is exhausted"
                        );
                        return Err(PlanningError::TemporarilyUnavailable);
                    }
                };
                self.plan_inner(request, deadline).await
            })
            .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    target: "via_server::routing",
                    timeout_ms = self.limits.request_timeout.as_millis().min(u128::from(u64::MAX)) as u64,
                    "journey planning timed out"
                );
                Err(PlanningError::TemporarilyUnavailable)
            }
        }
    }

    async fn plan_inner(
        &self,
        request: PlanningInput,
        deadline: PlanningDeadline,
    ) -> Result<PlanningOutcome, PlanningError> {
        let request_started_at = taipei_now();
        let context = JourneyPlanningContext::new(
            &request,
            request_started_at,
            self.limits.planner.max_journey_duration_seconds,
            deadline,
        )?;
        context.ensure_before_deadline()?;
        let (query, mut failures) = self.planner.discover_query(&context, &self.limits).await?;
        let search_started = Instant::now();
        let candidates = match self.search(query, &context).await {
            Ok((candidates, search_failures)) => {
                failures.merge(search_failures);
                let duration = search_started.elapsed();
                ::metrics::histogram!("via_planner_multimodal_search_duration_seconds")
                    .record(duration.as_secs_f64());
                tracing::debug!(
                    target: "via_server::routing",
                    multimodal_search_ms = duration.as_millis() as u64,
                    journey_candidates = candidates.len(),
                    "multimodal planning completed"
                );
                candidates
            }
            Err(RoutingError::DeadlineExceeded) => {
                return Err(PlanningError::TemporarilyUnavailable);
            }
            Err(error) => {
                failures.record_routing(&error);
                tracing::error!(error = %error, "transit planning failed");
                Vec::new()
            }
        };
        self.finalize_plan(request_started_at, &context, candidates, failures)
    }

    async fn search(
        &self,
        query: PlanningQuery,
        context: &JourneyPlanningContext<'_>,
    ) -> Result<(Vec<JourneyCandidate>, FailureSummary), RoutingError> {
        let planned = self.planner.plan(query).await?;
        context
            .ensure_before_deadline()
            .map_err(|_| RoutingError::DeadlineExceeded)?;
        let realtime_evidence_values: usize = planned
            .journeys
            .iter()
            .map(RealizedJourney::realtime_evidence_count)
            .sum();
        ::metrics::counter!("via_planner_transit_realtime_evidence_values_total")
            .increment(realtime_evidence_values as u64);
        let (candidates, reconstruction_failures) = self
            .reconstructor
            .reconstruct_all(
                &planned.journeys,
                context,
                self.limits.max_concurrent_reconstruction_candidates,
            )
            .await;
        let mut failures = planned.failures;
        failures.merge(reconstruction_failures);
        Ok((candidates, failures))
    }

    #[tracing::instrument(target = "via_profile", name = "Final Ranking", skip_all, fields(candidate_count = candidates.len()))]
    fn finalize_plan(
        &self,
        generated_at: OffsetDateTime,
        context: &JourneyPlanningContext<'_>,
        candidates: Vec<JourneyCandidate>,
        failures: FailureSummary,
    ) -> Result<PlanningOutcome, PlanningError> {
        tracing::debug!(
            target: "via_server::routing",
            journey_candidates = candidates.len(),
            "journey candidates ready for ranking"
        );
        if candidates.is_empty() && failures.internal {
            tracing::error!(
                target: "via_server::routing",
                "journey planning failed due to an internal routing error"
            );
            return Err(PlanningError::Internal);
        }
        if candidates.is_empty() && failures.transient {
            tracing::warn!(
                target: "via_server::routing",
                "journey planning has no result because a dependency was temporarily unavailable"
            );
            return Err(PlanningError::TemporarilyUnavailable);
        }
        context.ensure_before_deadline()?;
        let outcome = self.rank_candidates(generated_at, candidates)?;
        context.ensure_before_deadline()?;
        Ok(outcome)
    }

    fn rank_candidates(
        &self,
        generated_at: OffsetDateTime,
        candidates: Vec<JourneyCandidate>,
    ) -> Result<PlanningOutcome, PlanningError> {
        let mut seen_keys = HashSet::new();
        let candidates = candidates
            .into_iter()
            .filter(|candidate| seen_keys.insert(candidate.ranking.stable_key.clone()))
            .collect::<Vec<_>>();
        let candidates_before_ranking = candidates.len();
        let ranked_journeys = self.ranking.rank(
            candidates
                .iter()
                .map(|candidate| candidate.ranking.clone())
                .collect(),
        );
        tracing::debug!(
            target: "via_server::routing",
            candidates_before_ranking,
            candidates_after_ranking = ranked_journeys.len(),
            "journey candidates ranked"
        );
        let mut candidates_by_key = candidates
            .into_iter()
            .map(|candidate| (candidate.ranking.stable_key.clone(), candidate))
            .collect::<HashMap<_, _>>();
        let mut journey_options = Vec::with_capacity(ranked_journeys.len());
        for (index, ranked_journey) in ranked_journeys.into_iter().enumerate() {
            let Some(mut candidate) = candidates_by_key.remove(&ranked_journey.stable_key) else {
                tracing::error!(
                    target: "via_server::routing",
                    "ranking produced a candidate that could not be reconstructed"
                );
                return Err(PlanningError::Internal);
            };
            apply_ranked_journey(&mut candidate.option, &ranked_journey);
            assign_result_local_ids(&mut candidate.option, index);
            journey_options.push(candidate.option);
        }
        if journey_options.is_empty() {
            return Ok(PlanningOutcome::Empty(EmptyResult {
                generated_at,
                empty: EmptyReason {
                    code: "no_feasible_journey_option",
                },
            }));
        }
        Ok(PlanningOutcome::Options(OptionsResult {
            generated_at,
            journey_options,
        }))
    }
}

#[cfg(test)]
mod limits_tests {
    use super::*;
    use crate::{routing::model::PlannerLimits, street::StreetRoutingError};

    #[test]
    fn routing_configuration_preserves_zero_transfers() {
        let limits = JourneyPlanningLimits {
            planner: PlannerLimits {
                max_transfers: 0,
                ..PlannerLimits::default()
            },
            ..JourneyPlanningLimits::default()
        };
        assert_eq!(limits.normalized().planner.max_transfers, 0);
    }
    #[test]
    fn routing_and_query_share_planner_defaults() {
        let query = PlanningQuery::new(
            crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
            crate::street::Coordinate::try_new(25.001, 121.501).unwrap(),
            crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
            Vec::new(),
            Vec::new(),
            OffsetDateTime::now_utc(),
            crate::config::ViaConfig::default().planner_limits(),
        );
        assert_eq!(JourneyPlanningLimits::default().planner, query.limits);
        assert_eq!(query.limits, PlannerLimits::default());
    }

    #[test]
    fn direct_query_limits_preserve_explicit_zero_safety_bounds() {
        let limits = PlannerLimits {
            max_static_corridors: 0,
            ..PlannerLimits::default()
        };
        let query = PlanningQuery::new(
            crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
            crate::street::Coordinate::try_new(25.001, 121.501).unwrap(),
            crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
            Vec::new(),
            Vec::new(),
            OffsetDateTime::now_utc(),
            crate::config::ViaConfig::default().planner_limits(),
        )
        .with_limits(limits);
        assert_eq!(query.limits.max_static_corridors, 0);
        assert_eq!(
            JourneyPlanningLimits {
                planner: limits,
                ..JourneyPlanningLimits::default()
            }
            .normalized()
            .planner
            .max_static_corridors,
            1
        );
    }
    #[test]
    fn planning_failures_accumulate_without_treating_unreachable_as_failure() {
        let mut failures = FailureSummary::default();
        failures.record_provider(&StreetRoutingError::Unreachable);
        assert_eq!(failures, FailureSummary::default());
        failures.record_provider(&StreetRoutingError::Timeout);
        assert_eq!(
            failures,
            FailureSummary {
                transient: true,
                internal: false
            }
        );
        let mut internal = FailureSummary::default();
        internal.record_planning(&PlanningError::Internal);
        failures.merge(internal);
        assert_eq!(
            failures,
            FailureSummary {
                transient: true,
                internal: true
            }
        );
    }
}
