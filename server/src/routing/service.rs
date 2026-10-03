use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use time::OffsetDateTime;

use crate::{
    api::PlanningRequest,
    domain::{
        bikeshare::BikeShareNetwork,
        journey::{EmptyReason, EmptyResult, OptionsResult, PlanningError, PlanningOutcome},
        ranking::RankingPolicy,
        transit_runtime::TransitRuntime,
        transit_timing::TimingSnapshotLoader,
    },
    providers::BikeAvailabilitySource,
    street::{ProviderFailureClass, StreetRouter, StreetRoutingError},
};

use super::{
    model::{JourneyPlanningContext, JourneyPlanningLimits, PlanningDeadline, PlanningQuery},
    planner::MultimodalPlanner,
    realized::RealizedJourney,
    reconstruct::{
        JourneyCandidate, JourneyReconstructor, apply_ranked_journey, assign_result_local_ids,
        taipei_now, to_street_coordinate,
    },
};

#[derive(Clone)]
pub struct RoutingService {
    street_routing: Arc<dyn StreetRouter>,
    planner: MultimodalPlanner,
    reconstructor: JourneyReconstructor,
    ranking: RankingPolicy,
    limits: JourneyPlanningLimits,
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
    ) -> Self {
        let planner = MultimodalPlanner::new(transit_runtime.clone(), timing)
            .with_street_router(street_routing.clone())
            .with_bike_share(
                bike_share_network.clone(),
                bike_availability_source,
                street_routing.clone(),
            );
        let reconstructor =
            JourneyReconstructor::new(street_routing.clone(), transit_runtime, bike_share_network);
        let limits = limits.normalized();
        Self {
            street_routing,
            planner,
            reconstructor,
            ranking: RankingPolicy::default(),
            request_slots: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_requests)),
            limits,
        }
    }
}

impl RoutingService {
    pub async fn plan(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
        let timeout = self.limits.request_timeout;
        let deadline = PlanningDeadline::after(timeout);
        match tokio::time::timeout_at(deadline.instant(), async {
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
            self.plan_multimodal_inner(request, deadline).await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    target: "via_server::routing",
                    timeout_ms = timeout.as_millis().min(u128::from(u64::MAX)) as u64,
                    "journey planning timed out"
                );
                Err(PlanningError::TemporarilyUnavailable)
            }
        }
    }

    async fn plan_multimodal_inner(
        &self,
        request: PlanningRequest,
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

        let alternatives = self.plan_candidates(&context).await?;
        self.finalize_plan(
            request_started_at,
            &context,
            alternatives.candidates,
            alternatives.failures,
            alternatives.metrics,
        )
    }

    async fn plan_candidates(
        &self,
        context: &JourneyPlanningContext<'_>,
    ) -> Result<PlanningCandidates, PlanningError> {
        let request = context.request;
        let allowed_transit_modes = request.allowed_transit_modes();
        let mut alternatives = PlanningCandidates::default();

        let (origin, destination) = (
            to_street_coordinate(request.origin.coordinate).map_err(|_| PlanningError::Internal)?,
            to_street_coordinate(request.destination.coordinate)
                .map_err(|_| PlanningError::Internal)?,
        );
        let query = if allowed_transit_modes.is_empty() {
            PlanningQuery::new(Vec::new(), Vec::new(), context.search_start)
                .with_allowed_transit_modes([])
        } else {
            let access_started = Instant::now();
            let access = self
                .planner
                .discover_access_points(
                    self.street_routing.as_ref(),
                    request.origin.coordinate,
                    context.search_start,
                    context.access_constraints(),
                    &allowed_transit_modes,
                    &self.limits,
                )
                .await;
            context.ensure_before_deadline()?;
            let egress = self
                .planner
                .discover_egress_points(
                    self.street_routing.as_ref(),
                    request.destination.coordinate,
                    context.direct_constraints(),
                    &allowed_transit_modes,
                    &self.limits,
                )
                .await;
            context.ensure_before_deadline()?;
            alternatives.metrics.candidate_discovery_duration = Some(access_started.elapsed());
            let (access, egress) = match (access, egress) {
                (Ok(access), Ok(egress)) => (access, egress),
                (Err(error), _) | (_, Err(error)) => {
                    alternatives.failures.record_provider(&error);
                    (Vec::new(), Vec::new())
                }
            };
            alternatives.metrics.transit_candidate_stops = access.len() + egress.len();
            tracing::debug!(
                target: "via_server::routing",
                access = ?access.iter().map(|point| (
                    point.stop_id.as_str(),
                    self.planner.modes_at_stop(&point.stop_id),
                    point.walking_seconds,
                )).collect::<Vec<_>>(),
                egress = ?egress.iter().map(|point| (
                    point.stop_id.as_str(),
                    self.planner.modes_at_stop(&point.stop_id),
                    point.walking_seconds,
                )).collect::<Vec<_>>(),
                "discovered transit access and egress"
            );

            PlanningQuery::new(access, egress, context.search_start)
                .with_allowed_transit_modes(allowed_transit_modes)
        };
        let mut query = query
            .with_endpoints(origin, destination)
            .with_bike_share(request.allows_bike_share())
            .with_limits(self.limits.planner)
            .with_deadline(context.deadline);
        query.latest_arrival = context.latest_arrival;

        {
            let search_started = Instant::now();
            let planned = async {
                let mut session = self.planner.prepare(query).await?;
                let journeys = session.evaluate_primary().await?;
                context
                    .ensure_before_deadline()
                    .map_err(|_| super::model::RoutingError::DeadlineExceeded)?;
                alternatives.metrics.transit_realtime_evidence_values = journeys
                    .iter()
                    .map(RealizedJourney::realtime_evidence_count)
                    .sum();
                for journey in journeys {
                    match self.reconstructor.reconstruct(&journey, context).await {
                        Ok(Some(candidate)) => alternatives.candidates.push(candidate),
                        Ok(None) => {}
                        Err(error) => alternatives.failures.record_planning(&error),
                    }
                }
                if alternatives.candidates.is_empty() {
                    for journey in session.evaluate_fallback().await? {
                        match self.reconstructor.reconstruct(&journey, context).await {
                            Ok(Some(candidate)) => alternatives.candidates.push(candidate),
                            Ok(None) => {}
                            Err(error) => alternatives.failures.record_planning(&error),
                        }
                    }
                }
                alternatives.failures.transient |= session.failures().transient;
                alternatives.failures.internal |= session.failures().internal;
                Ok::<_, super::model::RoutingError>(session.stats().clone())
            }
            .await;
            match planned {
                Ok(planner_stats) => {
                    alternatives.metrics.transit_reachable_boardings =
                        planner_stats.phase1.static_corridor_positions;
                    alternatives.metrics.transit_reachable_patterns =
                        planner_stats.phase1.static_corridor_patterns;
                    alternatives.metrics.transit_realtime_stops_requested =
                        planner_stats.phase1.static_corridor_bus_stops;
                    alternatives.metrics.transit_departure_lookups =
                        planner_stats.phase2_evaluation.transit_departure_lookups;
                    alternatives.metrics.multimodal_search_duration =
                        Some(search_started.elapsed());
                }
                Err(error) => {
                    match &error {
                        super::model::RoutingError::Timing(timing) => {
                            alternatives.failures.transient |= timing.is_transient();
                        }
                        super::model::RoutingError::StaticSearchLimit
                        | super::model::RoutingError::Phase2EvaluationLimit => {
                            alternatives.failures.internal = true;
                        }
                        super::model::RoutingError::DeadlineExceeded => {
                            return Err(PlanningError::TemporarilyUnavailable);
                        }
                        super::model::RoutingError::InvalidQuery => {}
                    }
                    tracing::error!(error = %error, "transit planning failed");
                }
            }
        }

        Ok(alternatives)
    }

    fn finalize_plan(
        &self,
        generated_at: OffsetDateTime,
        context: &JourneyPlanningContext<'_>,
        candidates: Vec<JourneyCandidate>,
        failures: PlanningFailures,
        metrics: PlannerStageMetrics,
    ) -> Result<PlanningOutcome, PlanningError> {
        ::metrics::counter!("via_planner_transit_candidate_stops_total")
            .increment(metrics.transit_candidate_stops as u64);
        ::metrics::counter!("via_planner_transit_reachable_boardings_total")
            .increment(metrics.transit_reachable_boardings as u64);
        ::metrics::counter!("via_planner_transit_reachable_patterns_total")
            .increment(metrics.transit_reachable_patterns as u64);
        ::metrics::counter!("via_planner_transit_realtime_stops_requested_total")
            .increment(metrics.transit_realtime_stops_requested as u64);
        ::metrics::counter!("via_planner_transit_departure_lookups_total")
            .increment(metrics.transit_departure_lookups as u64);
        ::metrics::counter!("via_planner_transit_realtime_evidence_values_total")
            .increment(metrics.transit_realtime_evidence_values as u64);
        if let Some(duration) = metrics.candidate_discovery_duration {
            ::metrics::histogram!("via_planner_candidate_discovery_duration_seconds")
                .record(duration.as_secs_f64());
        }
        if let Some(duration) = metrics.multimodal_search_duration {
            ::metrics::histogram!("via_planner_multimodal_search_duration_seconds")
                .record(duration.as_secs_f64());
        }
        tracing::debug!(
            target: "via_server::routing",
            candidate_discovery_ms = metrics.candidate_discovery_duration.map_or(0, |duration| duration.as_millis() as u64),
            multimodal_search_ms = metrics.multimodal_search_duration.map_or(0, |duration| duration.as_millis() as u64),
            transit_candidate_stops = metrics.transit_candidate_stops,
            reachable_boardings = metrics.transit_reachable_boardings,
            reachable_patterns = metrics.transit_reachable_patterns,
            realtime_stops_requested = metrics.transit_realtime_stops_requested,
            transit_departure_lookups = metrics.transit_departure_lookups,
            realtime_evidence_values = metrics.transit_realtime_evidence_values,
            generated_candidates = candidates.len(),
            "journey planning routing stages"
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
        self.rank_generated(generated_at, candidates)
    }

    fn rank_generated(
        &self,
        generated_at: OffsetDateTime,
        generated: Vec<JourneyCandidate>,
    ) -> Result<PlanningOutcome, PlanningError> {
        let mut seen_keys = HashSet::new();
        let generated = generated
            .into_iter()
            .filter(|candidate| seen_keys.insert(candidate.ranking.stable_key.clone()))
            .collect::<Vec<_>>();
        let ranked = self.ranking.rank(
            generated
                .iter()
                .map(|candidate| candidate.ranking.clone())
                .collect(),
        );
        let mut by_key = generated
            .into_iter()
            .map(|candidate| (candidate.ranking.stable_key.clone(), candidate))
            .collect::<HashMap<_, _>>();
        let mut journey_options = Vec::with_capacity(ranked.len());
        for (index, ranked_journey) in ranked.into_iter().enumerate() {
            let Some(mut candidate) = by_key.remove(&ranked_journey.stable_key) else {
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

#[derive(Default)]
struct PlanningCandidates {
    candidates: Vec<JourneyCandidate>,
    failures: PlanningFailures,
    metrics: PlannerStageMetrics,
}

#[derive(Clone, Copy, Debug, Default)]
struct PlannerStageMetrics {
    transit_candidate_stops: usize,
    transit_reachable_boardings: usize,
    transit_reachable_patterns: usize,
    transit_realtime_stops_requested: usize,
    transit_departure_lookups: usize,
    transit_realtime_evidence_values: usize,
    candidate_discovery_duration: Option<StdDuration>,
    multimodal_search_duration: Option<StdDuration>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanningFailures {
    pub(crate) transient: bool,
    pub(crate) internal: bool,
}

impl PlanningFailures {
    fn record_provider(&mut self, error: &StreetRoutingError) {
        match error.failure_class() {
            ProviderFailureClass::Unreachable => {}
            ProviderFailureClass::Transient => self.transient = true,
            ProviderFailureClass::Internal => self.internal = true,
        }
    }

    fn record_planning(&mut self, error: &PlanningError) {
        self.transient |= *error == PlanningError::TemporarilyUnavailable;
        self.internal |= *error == PlanningError::Internal;
    }

    #[cfg(test)]
    fn merge(&mut self, other: Self) {
        self.transient |= other.transient;
        self.internal |= other.internal;
    }
}

#[cfg(test)]
mod limits_tests {
    use super::*;
    use crate::routing::model::PlannerLimits;

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
        let query = PlanningQuery::new(Vec::new(), Vec::new(), OffsetDateTime::now_utc());
        assert_eq!(JourneyPlanningLimits::default().planner, query.limits);
        assert_eq!(query.limits, PlannerLimits::default());
    }

    #[test]
    fn direct_query_limits_preserve_explicit_zero_safety_bounds() {
        let limits = PlannerLimits {
            max_phase2_expansions: 0,
            ..PlannerLimits::default()
        };
        let query = PlanningQuery::new(Vec::new(), Vec::new(), OffsetDateTime::now_utc())
            .with_limits(limits);
        assert_eq!(query.limits.max_phase2_expansions, 0);
        assert_eq!(
            JourneyPlanningLimits {
                planner: limits,
                ..JourneyPlanningLimits::default()
            }
            .normalized()
            .planner
            .max_phase2_expansions,
            1
        );
    }
    #[test]
    fn planning_failures_accumulate_without_treating_unreachable_as_failure() {
        let mut failures = PlanningFailures::default();
        failures.record_provider(&StreetRoutingError::Unreachable);
        assert_eq!(failures, PlanningFailures::default());
        failures.record_provider(&StreetRoutingError::Timeout);
        assert_eq!(
            failures,
            PlanningFailures {
                transient: true,
                internal: false
            }
        );
        let mut internal = PlanningFailures::default();
        internal.record_planning(&PlanningError::Internal);
        failures.merge(internal);
        assert_eq!(
            failures,
            PlanningFailures {
                transient: true,
                internal: true
            }
        );
    }
}
