use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        bikeshare::BikeShareNetwork,
        journey::{
            EmptyResult, JourneyOption, OptionsResult, PlanningError, PlanningOutcome,
            ResultMetadata, SemanticObject,
        },
        normalize_to_taipei,
        ranking::{RankingCandidate, RankingPolicy},
        transit_runtime::TransitRuntime,
        transit_timing::TimingSnapshotLoader,
    },
    observability::{OperationalMetrics, PlannerStageMetrics},
    providers::BikeAvailabilitySource,
    street::{
        Coordinate, RouteQuery, RoutingConstraints, RoutingTime, RoutingTimeKind, StreetPath,
        StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    access,
    model::{PlannerLimits, PlanningDeadline, TransitQuery},
    path::{JourneySkeleton, JourneyVariant},
    planner::{DirectBikeJourney, TransitPlanner},
    reconstruct::{
        apply_ranked_journey, assign_result_local_ids, taipei_now, to_street_coordinate,
    },
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
    pub(crate) operational_metrics: OperationalMetrics,
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
            operational_metrics: OperationalMetrics::default(),
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
    pub async fn route(
        &self,
        request: PlanningRequest,
        request_id: String,
    ) -> Result<PlanningOutcome, PlanningError> {
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
            self.plan_multimodal_inner(request, request_id, deadline)
                .await
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
        request_id: String,
        deadline: PlanningDeadline,
    ) -> Result<PlanningOutcome, PlanningError> {
        let request_started_at = taipei_now();
        let metadata = ResultMetadata::new(
            request_id,
            request_started_at,
            request
                .departure_setting
                .effective_search_time(request_started_at),
            self.transit_runtime.data().revision(),
        );
        let context = PlanningContext::new(
            &request,
            request_started_at,
            self.limits.planner.max_journey_duration_seconds,
            deadline,
        )?;
        context.ensure_before_deadline()?;
        let mut generated = Vec::new();
        let mut had_transient_failure = false;
        let mut had_global_failure = false;
        let mut stage_metrics = PlannerStageMetrics::default();

        let (direct_walking, walking_transient, walking_global) =
            self.discover_direct_walking(&context).await;
        context.ensure_before_deadline()?;
        had_transient_failure |= walking_transient;
        had_global_failure |= walking_global;
        let mut direct_bike_journey = None;
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
                direct_bike_journey = Some(DirectBikeJourney {
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
                        .reconstruct_multimodal_variant(&request, &variant, &context)
                        .await
                    {
                        Ok(Some(candidate)) => generated.push(candidate),
                        Ok(None) => {}
                        Err(error) => {
                            had_transient_failure |= error == PlanningError::TemporarilyUnavailable;
                            had_global_failure |= error == PlanningError::Internal;
                        }
                    }
                }
                Err(error) => {
                    tracing::error!(error = %error, "direct walking path could not be skeletonized");
                    had_global_failure = true;
                }
            }
        }

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
            stage_metrics.candidate_discovery_duration_ms =
                access_started.elapsed().as_millis() as u64;
            let (access, egress) = match (access, egress) {
                (Ok(access), Ok(egress)) => (access, egress),
                (Err(error), _) | (_, Err(error)) => {
                    had_transient_failure |= is_transient_provider_error(&error);
                    had_global_failure |= is_global_provider_error(&error);
                    (Vec::new(), Vec::new())
                }
            };
            stage_metrics.transit_candidate_stops = access.len() + egress.len();
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
                let query = TransitQuery {
                    origin_access: access,
                    destination_egress: egress,
                    allowed_transit_modes,
                    allow_bike_share: request.allows_bike_share(),
                    departure_time: context.search_start,
                    latest_arrival: context.latest_arrival,
                    limits: self.limits.planner,
                    deadline: Some(deadline),
                };
                let search_started = Instant::now();
                match self
                    .transit_planner
                    .plan_with_stats_and_direct(query, direct_bike_journey.take())
                    .await
                {
                    Ok((candidates, planner_stats)) => {
                        context.ensure_before_deadline()?;
                        stage_metrics.transit_reachable_boardings =
                            planner_stats.phase1.static_corridor_positions;
                        stage_metrics.transit_reachable_patterns =
                            planner_stats.phase1.static_corridor_patterns;
                        stage_metrics.transit_realtime_stops_requested =
                            planner_stats.phase1.static_corridor_bus_stops;
                        stage_metrics.transit_departure_lookups =
                            planner_stats.phase2_evaluation.transit_departure_lookups;
                        stage_metrics.transit_realtime_evidence_values = candidates
                            .iter()
                            .map(JourneyVariant::realtime_evidence_count)
                            .sum();
                        stage_metrics.multimodal_search_duration_ms =
                            search_started.elapsed().as_millis() as u64;
                        for variant in candidates {
                            match self
                                .reconstruct_multimodal_variant(&request, &variant, &context)
                                .await
                            {
                                Ok(Some(candidate)) => generated.push(candidate),
                                Ok(None) => {}
                                Err(error) => {
                                    had_transient_failure |=
                                        error == PlanningError::TemporarilyUnavailable;
                                    had_global_failure |= error == PlanningError::Internal;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        match &error {
                            super::model::RoutingError::Timing(timing) => {
                                had_transient_failure |= timing.is_transient();
                            }
                            super::model::RoutingError::StaticSearchLimit
                            | super::model::RoutingError::Phase2EvaluationLimit => {
                                had_global_failure = true;
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
        }

        if let Some(direct) = direct_bike_journey.take() {
            context.ensure_before_deadline()?;
            for variant in self
                .transit_planner
                .plan_direct_bike(direct, deadline)
                .await
            {
                context.ensure_before_deadline()?;
                match self
                    .reconstruct_multimodal_variant(&request, &variant, &context)
                    .await
                {
                    Ok(Some(candidate)) => generated.push(candidate),
                    Ok(None) => {}
                    Err(error) => {
                        had_transient_failure |= error == PlanningError::TemporarilyUnavailable;
                        had_global_failure |= error == PlanningError::Internal;
                    }
                }
            }
        }

        self.operational_metrics.record_planner_stage(stage_metrics);
        tracing::debug!(
            target: "via_server::routing",
            candidate_discovery_ms = stage_metrics.candidate_discovery_duration_ms,
            multimodal_search_ms = stage_metrics.multimodal_search_duration_ms,
            transit_candidate_stops = stage_metrics.transit_candidate_stops,
            reachable_boardings = stage_metrics.transit_reachable_boardings,
            reachable_patterns = stage_metrics.transit_reachable_patterns,
            realtime_stops_requested = stage_metrics.transit_realtime_stops_requested,
            transit_departure_lookups = stage_metrics.transit_departure_lookups,
            realtime_evidence_values = stage_metrics.transit_realtime_evidence_values,
            generated_candidates = generated.len(),
            "journey planning routing stages"
        );
        if had_global_failure {
            tracing::error!(
                target: "via_server::routing",
                "journey planning failed due to an internal routing error"
            );
            return Err(PlanningError::Internal);
        }
        if generated.is_empty() && had_transient_failure {
            tracing::warn!(
                target: "via_server::routing",
                "journey planning has no result because a dependency was temporarily unavailable"
            );
            return Err(PlanningError::TemporarilyUnavailable);
        }
        context.ensure_before_deadline()?;
        self.rank_generated(metadata, request, generated)
    }

    fn rank_generated(
        &self,
        metadata: ResultMetadata,
        request: PlanningRequest,
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
                metadata,
                empty: SemanticObject {
                    code: "no_feasible_journey_option",
                    required: true,
                    parameters: std::collections::BTreeMap::from([(
                        "selected_modes",
                        serde_json::to_value(request.canonical_selected_modes())
                            .unwrap_or_default(),
                    )]),
                },
            }));
        }
        Ok(PlanningOutcome::Options(OptionsResult {
            metadata,
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

impl PathAttempt {
    fn was_transient_failure(&self) -> bool {
        matches!(
            self,
            Self::Failed(
                StreetRoutingError::Timeout
                    | StreetRoutingError::RateLimited
                    | StreetRoutingError::TemporarilyUnavailable
            )
        )
    }

    fn is_global_failure(&self) -> bool {
        matches!(
            self,
            Self::Failed(
                StreetRoutingError::InvalidRequest { .. }
                    | StreetRoutingError::MalformedResponse
                    | StreetRoutingError::ResponseTooLarge { .. }
            )
        )
    }
}

pub(crate) fn is_transient_provider_error(error: &StreetRoutingError) -> bool {
    matches!(
        error,
        StreetRoutingError::Timeout
            | StreetRoutingError::RateLimited
            | StreetRoutingError::TemporarilyUnavailable
    )
}

pub(crate) fn is_global_provider_error(error: &StreetRoutingError) -> bool {
    matches!(
        error,
        StreetRoutingError::InvalidRequest { .. }
            | StreetRoutingError::MalformedResponse
            | StreetRoutingError::ResponseTooLarge { .. }
    )
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

    async fn discover_direct_walking(
        &self,
        context: &PlanningContext,
    ) -> (Option<DirectWalking>, bool, bool) {
        let (Ok(origin), Ok(destination)) = (
            to_street_coordinate(context.request.origin.coordinate),
            to_street_coordinate(context.request.destination.coordinate),
        ) else {
            return (None, false, false);
        };
        let attempt = self
            .street_path(
                StreetTravelMode::Walking,
                origin,
                destination,
                context.direct_constraints(),
            )
            .await;
        let transient = attempt.was_transient_failure();
        let global = attempt.is_global_failure();
        let PathAttempt::Found(path) = attempt else {
            return (None, transient, global);
        };
        (Some(DirectWalking { path }), false, false)
    }
}

impl From<StreetRoutingError> for PlanningError {
    fn from(error: StreetRoutingError) -> Self {
        match error {
            StreetRoutingError::Timeout
            | StreetRoutingError::RateLimited
            | StreetRoutingError::TemporarilyUnavailable => Self::TemporarilyUnavailable,
            StreetRoutingError::Unreachable => unreachable_is_handled_before_conversion(),
            StreetRoutingError::InvalidRequest { .. }
            | StreetRoutingError::MalformedResponse
            | StreetRoutingError::ResponseTooLarge { .. } => Self::Internal,
        }
    }
}

fn unreachable_is_handled_before_conversion() -> ! {
    panic!("unreachable street route must be handled before conversion")
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
}
