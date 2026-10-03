use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        bikeshare::BikeShareNetwork,
        journey::{EmptyReason, EmptyResult, OptionsResult, PlanningError, PlanningOutcome},
        normalize_to_taipei,
        ranking::RankingPolicy,
        transit_runtime::TransitRuntime,
        transit_timing::TimingSnapshotLoader,
    },
    providers::BikeAvailabilitySource,
    street::{
        Coordinate, ProviderFailureClass, RouteQuery, RoutingConstraints, StreetPath, StreetRouter,
        StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    model::{JourneyPlanningContext, JourneyPlanningLimits, PlanningDeadline, TransitQuery},
    planner::{DirectBikeCandidate, TransitPlanner},
    reconstruct::{
        JourneyCandidate, JourneyReconstructor, apply_ranked_journey, assign_result_local_ids,
        taipei_now, to_street_coordinate,
    },
    variant::{JourneySkeleton, JourneyVariant},
};

#[derive(Clone)]
pub struct RoutingService {
    street_routing: Arc<dyn StreetRouter>,
    transit_planner: TransitPlanner,
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
        let transit_planner = TransitPlanner::new(transit_runtime.clone(), timing).with_bike_share(
            bike_share_network.clone(),
            bike_availability_source,
            street_routing.clone(),
        );
        let reconstructor =
            JourneyReconstructor::new(street_routing.clone(), transit_runtime, bike_share_network);
        let limits = limits.normalized();
        Self {
            street_routing,
            transit_planner,
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

        let direct = self.discover_direct_alternatives(&context).await?;
        let transit = self
            .discover_transit_alternatives(&context, direct.direct_bike)
            .await?;
        let candidates = direct
            .candidates
            .into_iter()
            .chain(transit.candidates)
            .collect();
        let mut failures = direct.failures;
        failures.merge(transit.failures);

        self.finalize_plan(
            request_started_at,
            &context,
            candidates,
            failures,
            transit.metrics,
        )
    }

    async fn discover_direct_alternatives(
        &self,
        context: &JourneyPlanningContext<'_>,
    ) -> Result<DirectAlternatives, PlanningError> {
        let request = context.request;
        let walking = self.discover_direct_walking(context).await;
        context.ensure_before_deadline()?;
        let mut alternatives = DirectAlternatives::default();
        alternatives.failures.merge(walking.failures);

        if let Some(path) = walking.walking {
            let departure_at = match request.departure_setting {
                crate::api::DepartureSetting::ArriveBy { time } => {
                    normalize_to_taipei(time) - Duration::seconds(i64::from(path.duration.0))
                }
                crate::api::DepartureSetting::DepartNow
                | crate::api::DepartureSetting::DepartAt { .. } => context.search_start,
            };
            if request.allows_bike_share()
                && let (Ok(origin), Ok(destination)) = (
                    to_street_coordinate(request.origin.coordinate),
                    to_street_coordinate(request.destination.coordinate),
                )
            {
                alternatives.direct_bike = Some(DirectBikeCandidate {
                    path: path.clone(),
                    origin,
                    destination,
                    departure_at,
                });
            }
            match JourneySkeleton::from_direct_walking(path, departure_at) {
                Ok(skeleton) => {
                    let variant = JourneyVariant::baseline(skeleton);
                    match self.reconstructor.reconstruct(&variant, context).await {
                        Ok(Some(candidate)) => alternatives.candidates.push(candidate),
                        Ok(None) => {}
                        Err(error) => alternatives.failures.record_planning(&error),
                    }
                }
                Err(error) => {
                    tracing::error!(error = %error, "direct walking path could not be skeletonized");
                    alternatives.failures.internal = true;
                }
            }
        }

        Ok(alternatives)
    }

    async fn discover_transit_alternatives(
        &self,
        context: &JourneyPlanningContext<'_>,
        direct_bike: Option<DirectBikeCandidate>,
    ) -> Result<TransitAlternatives, PlanningError> {
        let request = context.request;
        let allowed_transit_modes = request.allowed_transit_modes();
        let mut alternatives = TransitAlternatives::default();
        let mut direct_bike = direct_bike;

        let query = if allowed_transit_modes.is_empty() {
            None
        } else {
            let access_started = Instant::now();
            let access = self
                .transit_planner
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
                .transit_planner
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
                    self.transit_planner.modes_at_stop(&point.stop_id),
                    point.walking_seconds,
                )).collect::<Vec<_>>(),
                egress = ?egress.iter().map(|point| (
                    point.stop_id.as_str(),
                    self.transit_planner.modes_at_stop(&point.stop_id),
                    point.walking_seconds,
                )).collect::<Vec<_>>(),
                "discovered transit access and egress"
            );

            if !access.is_empty() && !egress.is_empty() {
                Some(TransitQuery {
                    origin_access: access,
                    destination_egress: egress,
                    allowed_transit_modes,
                    allow_bike_share: request.allows_bike_share(),
                    departure_time: context.search_start,
                    latest_arrival: context.latest_arrival,
                    limits: self.limits.planner,
                    deadline: Some(context.deadline),
                })
            } else {
                None
            }
        };

        if let Some(query) = query {
            let search_started = Instant::now();
            match self
                .transit_planner
                .plan_with_stats_and_direct(query, direct_bike.take())
                .await
            {
                Ok((variants, planner_stats)) => {
                    context.ensure_before_deadline()?;
                    alternatives.metrics.transit_reachable_boardings =
                        planner_stats.phase1.static_corridor_positions;
                    alternatives.metrics.transit_reachable_patterns =
                        planner_stats.phase1.static_corridor_patterns;
                    alternatives.metrics.transit_realtime_stops_requested =
                        planner_stats.phase1.static_corridor_bus_stops;
                    alternatives.metrics.transit_departure_lookups =
                        planner_stats.phase2_evaluation.transit_departure_lookups;
                    alternatives.metrics.transit_realtime_evidence_values = variants
                        .iter()
                        .map(JourneyVariant::realtime_evidence_count)
                        .sum();
                    alternatives.metrics.multimodal_search_duration =
                        Some(search_started.elapsed());
                    for variant in variants {
                        match self.reconstructor.reconstruct(&variant, context).await {
                            Ok(Some(candidate)) => alternatives.candidates.push(candidate),
                            Ok(None) => {}
                            Err(error) => alternatives.failures.record_planning(&error),
                        }
                    }
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

        if let Some(direct) = direct_bike {
            context.ensure_before_deadline()?;
            for variant in self
                .transit_planner
                .plan_direct_bike(direct, context.deadline)
                .await
            {
                context.ensure_before_deadline()?;
                match self.reconstructor.reconstruct(&variant, context).await {
                    Ok(Some(candidate)) => alternatives.candidates.push(candidate),
                    Ok(None) => {}
                    Err(error) => alternatives.failures.record_planning(&error),
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
        if failures.internal {
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
struct DirectAlternatives {
    candidates: Vec<JourneyCandidate>,
    direct_bike: Option<DirectBikeCandidate>,
    failures: PlanningFailures,
}

#[derive(Default)]
struct TransitAlternatives {
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

enum PathAttempt {
    Found(StreetPath),
    Unreachable,
    Failed(StreetRoutingError),
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

    fn merge(&mut self, other: Self) {
        self.transient |= other.transient;
        self.internal |= other.internal;
    }
}

#[derive(Default)]
struct DirectWalkingOutcome {
    walking: Option<StreetPath>,
    failures: PlanningFailures,
}

impl RoutingService {
    async fn street_path(
        &self,
        mode: StreetTravelMode,
        origin: Coordinate,
        destination: Coordinate,
        constraints: RoutingConstraints,
    ) -> PathAttempt {
        match self
            .street_routing
            .route(RouteQuery {
                travel_mode: mode,
                origin,
                destination,
                constraints,
            })
            .await
        {
            Ok(path) => PathAttempt::Found(path),
            Err(StreetRoutingError::Unreachable) => PathAttempt::Unreachable,
            Err(error) => PathAttempt::Failed(error),
        }
    }

    async fn discover_direct_walking(
        &self,
        context: &JourneyPlanningContext<'_>,
    ) -> DirectWalkingOutcome {
        let (Ok(origin), Ok(destination)) = (
            to_street_coordinate(context.request.origin.coordinate),
            to_street_coordinate(context.request.destination.coordinate),
        ) else {
            return DirectWalkingOutcome::default();
        };
        let attempt = self
            .street_path(
                StreetTravelMode::Walking,
                origin,
                destination,
                context.direct_constraints(),
            )
            .await;
        match attempt {
            PathAttempt::Found(path) => DirectWalkingOutcome {
                walking: Some(path),
                failures: PlanningFailures::default(),
            },
            PathAttempt::Unreachable => DirectWalkingOutcome::default(),
            PathAttempt::Failed(error) => {
                let mut failures = PlanningFailures::default();
                failures.record_provider(&error);
                DirectWalkingOutcome {
                    walking: None,
                    failures,
                }
            }
        }
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
        let query = TransitQuery::new(Vec::new(), Vec::new(), OffsetDateTime::now_utc());
        assert_eq!(JourneyPlanningLimits::default().planner, query.limits);
        assert_eq!(query.limits, PlannerLimits::default());
    }

    #[test]
    fn direct_query_limits_preserve_explicit_zero_safety_bounds() {
        let limits = PlannerLimits {
            max_phase2_expansions: 0,
            ..PlannerLimits::default()
        };
        let query = TransitQuery::new(Vec::new(), Vec::new(), OffsetDateTime::now_utc())
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
