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
        journey::{
            EmptyReason, EmptyResult, JourneyOption, OptionsResult, PlanningError, PlanningOutcome,
        },
        normalize_to_taipei,
        ranking::{RankingCandidate, RankingPolicy},
        transit_runtime::TransitRuntime,
        transit_timing::TimingSnapshotLoader,
    },
    providers::BikeAvailabilitySource,
    street::{
        Coordinate, ProviderFailureClass, RouteQuery, RoutingConstraints, RoutingTime,
        RoutingTimeKind, StreetPath, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    access,
    model::{PlannerLimits, PlanningDeadline, TransitQuery},
    planner::{DirectBikeJourney, TransitPlanner},
    reconstruct::{
        apply_ranked_journey, assign_result_local_ids, taipei_now, to_street_coordinate,
    },
    variant::{JourneySkeleton, JourneyVariant},
};

#[derive(Clone)]
pub struct RoutingService {
    pub(crate) street_routing: Arc<dyn StreetRouter>,
    pub(crate) transit_planner: TransitPlanner,
    pub(crate) transit_runtime: Arc<TransitRuntime>,
    pub(crate) bike_share_network: Arc<BikeShareNetwork>,
    pub(crate) ranking: RankingPolicy,
    pub(crate) limits: RoutingLimits,
    pub(crate) request_slots: Arc<tokio::sync::Semaphore>,
}

#[derive(Clone, Debug)]
pub struct RoutingLimits {
    pub planner: PlannerLimits,
    pub request_timeout: std::time::Duration,
    pub max_concurrent_requests: usize,
    pub max_transit_access_stops: usize,
    pub max_transit_egress_stops: usize,
    pub max_walking_radius_metres: u32,
    pub max_walking_seconds: u32,
}

impl Default for RoutingLimits {
    fn default() -> Self {
        Self {
            planner: PlannerLimits::default(),
            request_timeout: std::time::Duration::from_secs(5),
            max_concurrent_requests: 32,
            max_transit_access_stops: 24,
            max_transit_egress_stops: 24,
            max_walking_radius_metres: 2_000,
            max_walking_seconds: 30 * 60,
        }
    }
}

impl RoutingService {
    pub fn new(
        street_routing: Arc<dyn StreetRouter>,
        transit_runtime: Arc<TransitRuntime>,
        timing: Arc<dyn TimingSnapshotLoader>,
        bike_share_network: Arc<BikeShareNetwork>,
        bike_availability_source: Arc<dyn BikeAvailabilitySource>,
        limits: RoutingLimits,
    ) -> Self {
        let transit_planner = TransitPlanner::new(transit_runtime.clone(), timing).with_bike_share(
            bike_share_network.clone(),
            bike_availability_source,
            street_routing.clone(),
        );
        let limits = limits.normalized();
        Self {
            street_routing: street_routing.clone(),
            transit_planner,
            transit_runtime,
            bike_share_network,
            ranking: RankingPolicy::default(),
            request_slots: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_requests)),
            limits,
        }
    }
}

impl RoutingLimits {
    fn normalized(self) -> Self {
        Self {
            planner: self.planner.normalized(),
            request_timeout: self.request_timeout,
            max_concurrent_requests: self.max_concurrent_requests.max(1),
            max_transit_access_stops: self.max_transit_access_stops.max(1),
            max_transit_egress_stops: self.max_transit_egress_stops.max(1),
            max_walking_radius_metres: self.max_walking_radius_metres.max(1),
            max_walking_seconds: self.max_walking_seconds.max(1),
        }
    }
}

impl RoutingService {
    pub async fn route(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
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
        let context = PlanningContext::new(
            &request,
            request_started_at,
            self.limits.planner.max_journey_duration_seconds,
            deadline,
        )?;
        context.ensure_before_deadline()?;
        let mut state = PlanningState::default();
        self.discover_direct_alternatives(&context, &mut state)
            .await?;
        let transit_query = self
            .discover_transit_endpoints(&context, &mut state)
            .await?;
        self.plan_and_reconstruct(transit_query, &context, &mut state)
            .await?;
        self.finalize_plan(request_started_at, &context, state)
    }

    async fn discover_direct_alternatives(
        &self,
        context: &PlanningContext,
        state: &mut PlanningState,
    ) -> Result<(), PlanningError> {
        let request = &context.request;
        let DirectWalkingOutcome {
            walking: direct_walking,
            failures: walking_failures,
        } = self.discover_direct_walking(context).await;
        context.ensure_before_deadline()?;
        state.failures.merge(walking_failures);
        if let Some(direct_walking) = &direct_walking {
            let departure_at = match request.departure_setting {
                crate::api::DepartureSetting::ArriveBy { time } => {
                    normalize_to_taipei(time)
                        - Duration::seconds(i64::from(direct_walking.path.duration.0))
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
                state.direct_bike = Some(DirectBikeJourney {
                    path: direct_walking.path.clone(),
                    origin,
                    destination,
                    departure_at,
                });
            }
            match JourneySkeleton::from_direct_walking(direct_walking.path.clone(), departure_at) {
                Ok(skeleton) => {
                    let variant = JourneyVariant::baseline(skeleton);
                    match self
                        .reconstruct_multimodal_variant(request, &variant, context)
                        .await
                    {
                        Ok(Some(candidate)) => state.candidates.push(candidate),
                        Ok(None) => {}
                        Err(error) => {
                            state.failures.record_planning(&error);
                        }
                    }
                }
                Err(error) => {
                    tracing::error!(error = %error, "direct walking path could not be skeletonized");
                    state.failures.internal = true;
                }
            }
        }

        Ok(())
    }

    async fn discover_transit_endpoints(
        &self,
        context: &PlanningContext,
        state: &mut PlanningState,
    ) -> Result<Option<TransitQuery>, PlanningError> {
        let request = &context.request;
        let allowed_transit_modes = request.allowed_transit_modes();
        if !allowed_transit_modes.is_empty() {
            let access_started = Instant::now();
            let access = access::discover_access_points(
                self.street_routing.as_ref(),
                access::AccessNetwork {
                    runtime: &self.transit_runtime,
                    topology: self.transit_planner.topology(),
                },
                request.origin.coordinate,
                context.search_start,
                context.access_constraints(),
                &allowed_transit_modes,
                &self.limits,
            )
            .await;
            context.ensure_before_deadline()?;
            let egress = access::discover_egress_points(
                self.street_routing.as_ref(),
                access::AccessNetwork {
                    runtime: &self.transit_runtime,
                    topology: self.transit_planner.topology(),
                },
                request.destination.coordinate,
                context.direct_constraints(),
                &allowed_transit_modes,
                &self.limits,
            )
            .await;
            context.ensure_before_deadline()?;
            state.metrics.candidate_discovery_duration = Some(access_started.elapsed());
            let (access, egress) = match (access, egress) {
                (Ok(access), Ok(egress)) => (access, egress),
                (Err(error), _) | (_, Err(error)) => {
                    state.failures.record_provider(&error);
                    (Vec::new(), Vec::new())
                }
            };
            state.metrics.transit_candidate_stops = access.len() + egress.len();
            tracing::debug!(
                target: "via_server::routing",
                access = ?access.iter().map(|point| (
                    point.stop_id.as_str(),
                    access::modes_at_stop(
                        &self.transit_runtime,
                        self.transit_planner.topology(),
                        &point.stop_id,
                    ),
                    point.walking_seconds,
                )).collect::<Vec<_>>(),
                egress = ?egress.iter().map(|point| (
                    point.stop_id.as_str(),
                    access::modes_at_stop(
                        &self.transit_runtime,
                        self.transit_planner.topology(),
                        &point.stop_id,
                    ),
                    point.walking_seconds,
                )).collect::<Vec<_>>(),
                "discovered transit access and egress"
            );

            if !access.is_empty() && !egress.is_empty() {
                return Ok(Some(TransitQuery {
                    origin_access: access,
                    destination_egress: egress,
                    allowed_transit_modes,
                    allow_bike_share: request.allows_bike_share(),
                    departure_time: context.search_start,
                    latest_arrival: context.latest_arrival,
                    limits: self.limits.planner,
                    deadline: Some(context.deadline),
                }));
            }
        }
        Ok(None)
    }

    async fn plan_and_reconstruct(
        &self,
        query: Option<TransitQuery>,
        context: &PlanningContext,
        state: &mut PlanningState,
    ) -> Result<(), PlanningError> {
        let request = &context.request;
        if let Some(query) = query {
            let search_started = Instant::now();
            match self
                .transit_planner
                .plan_with_stats_and_direct(query, state.direct_bike.take())
                .await
            {
                Ok((candidates, planner_stats)) => {
                    context.ensure_before_deadline()?;
                    state.metrics.transit_reachable_boardings =
                        planner_stats.phase1.static_corridor_positions;
                    state.metrics.transit_reachable_patterns =
                        planner_stats.phase1.static_corridor_patterns;
                    state.metrics.transit_realtime_stops_requested =
                        planner_stats.phase1.static_corridor_bus_stops;
                    state.metrics.transit_departure_lookups =
                        planner_stats.phase2_evaluation.transit_departure_lookups;
                    state.metrics.transit_realtime_evidence_values = candidates
                        .iter()
                        .map(JourneyVariant::realtime_evidence_count)
                        .sum();
                    state.metrics.multimodal_search_duration = Some(search_started.elapsed());
                    for variant in candidates {
                        match self
                            .reconstruct_multimodal_variant(request, &variant, context)
                            .await
                        {
                            Ok(Some(candidate)) => state.candidates.push(candidate),
                            Ok(None) => {}
                            Err(error) => {
                                state.failures.record_planning(&error);
                            }
                        }
                    }
                }
                Err(error) => {
                    match &error {
                        super::model::RoutingError::Timing(timing) => {
                            state.failures.transient |= timing.is_transient();
                        }
                        super::model::RoutingError::StaticSearchLimit
                        | super::model::RoutingError::Phase2EvaluationLimit => {
                            state.failures.internal = true;
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
        if let Some(direct) = state.direct_bike.take() {
            context.ensure_before_deadline()?;
            for variant in self
                .transit_planner
                .plan_direct_bike(direct, context.deadline)
                .await
            {
                context.ensure_before_deadline()?;
                match self
                    .reconstruct_multimodal_variant(request, &variant, context)
                    .await
                {
                    Ok(Some(candidate)) => state.candidates.push(candidate),
                    Ok(None) => {}
                    Err(error) => {
                        state.failures.record_planning(&error);
                    }
                }
            }
        }

        Ok(())
    }

    fn finalize_plan(
        &self,
        generated_at: OffsetDateTime,
        context: &PlanningContext,
        state: PlanningState,
    ) -> Result<PlanningOutcome, PlanningError> {
        let metrics = state.metrics;
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
            generated_candidates = state.candidates.len(),
            "journey planning routing stages"
        );
        if state.failures.internal {
            tracing::error!(
                target: "via_server::routing",
                "journey planning failed due to an internal routing error"
            );
            return Err(PlanningError::Internal);
        }
        if state.candidates.is_empty() && state.failures.transient {
            tracing::warn!(
                target: "via_server::routing",
                "journey planning has no result because a dependency was temporarily unavailable"
            );
            return Err(PlanningError::TemporarilyUnavailable);
        }
        context.ensure_before_deadline()?;
        self.rank_generated(generated_at, state.candidates)
    }

    fn rank_generated(
        &self,
        generated_at: OffsetDateTime,
        generated: Vec<ReconstructedCandidate>,
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

#[derive(Clone)]
pub(crate) struct PlanningContext {
    pub(crate) request: PlanningRequest,
    pub(crate) search_start: OffsetDateTime,
    pub(crate) latest_arrival: Option<OffsetDateTime>,
    pub(crate) deadline: PlanningDeadline,
}

impl PlanningContext {
    pub(crate) fn new(
        request: &PlanningRequest,
        request_started_at: OffsetDateTime,
        max_journey_duration_seconds: u32,
        deadline: PlanningDeadline,
    ) -> Result<Self, PlanningError> {
        let effective_search_time = request
            .departure_setting
            .effective_search_time(request_started_at);
        let (search_start, latest_arrival) = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { .. } => (
                effective_search_time - Duration::seconds(i64::from(max_journey_duration_seconds)),
                Some(effective_search_time),
            ),
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => (effective_search_time, None),
        };
        Ok(Self {
            request: request.clone(),
            search_start,
            latest_arrival,
            deadline,
        })
    }

    pub(crate) fn ensure_before_deadline(&self) -> Result<(), PlanningError> {
        if self.deadline.is_elapsed() {
            Err(PlanningError::TemporarilyUnavailable)
        } else {
            Ok(())
        }
    }

    pub(crate) fn local_time(&self, instant: OffsetDateTime) -> time::PrimitiveDateTime {
        let instant = normalize_to_taipei(instant);
        instant.date().with_time(instant.time())
    }

    pub(crate) fn constraints(
        &self,
        instant: OffsetDateTime,
        kind: RoutingTimeKind,
    ) -> RoutingConstraints {
        RoutingConstraints {
            time: Some(RoutingTime {
                kind,
                local_date_time: self.local_time(instant),
            }),
            max_distance: None,
        }
    }

    pub(crate) fn access_constraints(&self) -> RoutingConstraints {
        self.constraints(self.search_start, RoutingTimeKind::DepartAt)
    }

    pub(crate) fn direct_constraints(&self) -> RoutingConstraints {
        match self.latest_arrival {
            Some(latest_arrival) => self.constraints(latest_arrival, RoutingTimeKind::ArriveBy),
            None => self.constraints(self.search_start, RoutingTimeKind::DepartAt),
        }
    }
}

#[derive(Default)]
struct PlanningState {
    candidates: Vec<ReconstructedCandidate>,
    failures: PlanningFailures,
    metrics: PlannerStageMetrics,
    direct_bike: Option<DirectBikeJourney>,
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

pub(crate) struct ReconstructedCandidate {
    pub(crate) ranking: RankingCandidate,
    pub(crate) option: JourneyOption,
}

pub(crate) struct DirectWalking {
    pub(crate) path: StreetPath,
}

pub(crate) enum PathAttempt {
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
    walking: Option<DirectWalking>,
    failures: PlanningFailures,
}

impl RoutingService {
    pub(crate) async fn street_path(
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

    async fn discover_direct_walking(&self, context: &PlanningContext) -> DirectWalkingOutcome {
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
                walking: Some(DirectWalking { path }),
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

    #[test]
    fn routing_configuration_preserves_zero_transfers() {
        let limits = RoutingLimits {
            planner: PlannerLimits {
                max_transfers: 0,
                ..PlannerLimits::default()
            },
            ..RoutingLimits::default()
        };
        assert_eq!(limits.normalized().planner.max_transfers, 0);
    }
    #[test]
    fn routing_and_query_share_planner_defaults() {
        let query = TransitQuery::new(Vec::new(), Vec::new(), OffsetDateTime::now_utc());
        assert_eq!(RoutingLimits::default().planner, query.limits);
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
            RoutingLimits {
                planner: limits,
                ..RoutingLimits::default()
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
