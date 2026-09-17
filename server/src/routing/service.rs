use std::{collections::HashMap, sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::{
    api::{PlanningRequest, new_request_id},
    domain::{
        journey::{
            EmptyResult, JourneyMode, JourneyOption, OptionsResult, PlanningError, PlanningOutcome,
            ResultMetadata, SemanticObject,
        },
        normalize_to_taipei,
        ranking::{CandidateJourney, RankingPolicy},
        transit::TransitNetwork,
        transit_timing::DepartureResolver,
    },
    observability::{OperationalMetrics, PlannerStageMetrics},
    providers::realtime::RealtimeProvider,
    street::{
        Coordinate, RouteQuery, RoutingConstraints, RoutingTime, RoutingTimeKind, StreetPath,
        StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    access,
    algorithm::{AlgorithmQuery, JourneyCandidate, RoutingAlgorithm},
    bike::{BikeAvailabilityResolver, EndpointPath},
    reconstruct::{
        apply_ranked_journey, assign_result_local_ids, taipei_now, to_street_coordinate,
    },
    status,
    transfers::{TransferIndex, TransferShortcutIndex},
    trip_based::TripBasedRouter,
};

#[derive(Clone)]
pub struct RoutingService {
    pub(crate) street_routing: Arc<dyn StreetRouter>,
    pub(crate) departure_resolver: Arc<dyn DepartureResolver>,
    pub(crate) routing_algorithm: Arc<dyn RoutingAlgorithm>,
    pub(crate) transfer_index: Arc<dyn TransferIndex>,
    pub(crate) bike_availability: Option<Arc<dyn BikeAvailabilityResolver>>,
    pub(crate) realtime: Arc<dyn RealtimeProvider>,
    pub(crate) transit_network: Arc<TransitNetwork>,
    pub(crate) ranking: RankingPolicy,
    pub(crate) limits: RoutingLimits,
    pub(crate) request_slots: Arc<tokio::sync::Semaphore>,
    pub(crate) operational_metrics: OperationalMetrics,
}

#[derive(Clone, Debug)]
pub struct RoutingLimits {
    pub request_timeout: std::time::Duration,
    pub max_concurrent_requests: usize,
    pub max_transit_access_stops: usize,
    pub max_transit_egress_stops: usize,
    pub max_candidates: usize,
    pub max_walking_radius_metres: u32,
    pub max_walking_seconds: u32,
    pub max_bike_search_radius_metres: u32,
    pub max_bike_pickup_stations: usize,
    pub max_bike_return_stations: usize,
    pub max_bike_connectors: usize,
    pub max_bike_distance_metres: u32,
    pub max_bike_duration_seconds: u32,
    pub max_bike_extra_seconds: u32,
    pub max_transfer_count: u32,
    pub max_search_expansions: usize,
    pub max_departures_per_query: usize,
    pub max_journey_duration_seconds: u32,
}

impl Default for RoutingLimits {
    fn default() -> Self {
        Self {
            request_timeout: std::time::Duration::from_secs(5),
            max_concurrent_requests: 32,
            max_transit_access_stops: 24,
            max_transit_egress_stops: 24,
            max_candidates: 16,
            max_walking_radius_metres: 2_000,
            max_walking_seconds: 30 * 60,
            max_bike_search_radius_metres: 2_000,
            max_bike_pickup_stations: 6,
            max_bike_return_stations: 6,
            max_bike_connectors: 24,
            max_bike_distance_metres: 20_000,
            max_bike_duration_seconds: 2 * 60 * 60,
            max_bike_extra_seconds: 5 * 60,
            max_transfer_count: 4,
            max_search_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
        }
    }
}

impl RoutingService {
    pub fn new(
        street_routing: Arc<dyn StreetRouter>,
        departure_resolver: Arc<dyn DepartureResolver>,
        realtime: Arc<dyn RealtimeProvider>,
    ) -> Self {
        let transit_network = Arc::new(TransitNetwork::default());
        let transfer_index: Arc<dyn TransferIndex> =
            Arc::new(TransferShortcutIndex::from_network(&transit_network));
        let routing_algorithm: Arc<dyn RoutingAlgorithm> = Arc::new(TripBasedRouter::new(
            transit_network.clone(),
            departure_resolver.clone(),
            transfer_index.clone(),
        ));
        Self {
            street_routing,
            departure_resolver,
            routing_algorithm,
            transfer_index,
            bike_availability: None,
            realtime,
            transit_network,
            ranking: RankingPolicy::default(),
            request_slots: Arc::new(tokio::sync::Semaphore::new(
                RoutingLimits::default().max_concurrent_requests,
            )),
            limits: RoutingLimits::default(),
            operational_metrics: OperationalMetrics::default(),
        }
    }

    pub fn with_transit_network(mut self, transit_network: TransitNetwork) -> Self {
        self.transit_network = Arc::new(transit_network);
        self.transfer_index = Arc::new(TransferShortcutIndex::from_network(&self.transit_network));
        self.rebuild_default_algorithm();
        self
    }

    pub fn with_transit_network_arc(mut self, transit_network: Arc<TransitNetwork>) -> Self {
        self.transit_network = transit_network;
        self.transfer_index = Arc::new(TransferShortcutIndex::from_network(&self.transit_network));
        self.rebuild_default_algorithm();
        self
    }

    pub fn with_departure_resolver(mut self, resolver: Arc<dyn DepartureResolver>) -> Self {
        self.departure_resolver = resolver;
        self.rebuild_default_algorithm();
        self
    }

    pub fn with_routing_algorithm(mut self, algorithm: Arc<dyn RoutingAlgorithm>) -> Self {
        self.routing_algorithm = algorithm;
        self
    }

    pub fn with_bike_availability(mut self, resolver: Arc<dyn BikeAvailabilityResolver>) -> Self {
        self.bike_availability = Some(resolver);
        self.rebuild_default_algorithm();
        self
    }

    fn rebuild_default_algorithm(&mut self) {
        let router = TripBasedRouter::new(
            self.transit_network.clone(),
            self.departure_resolver.clone(),
            self.transfer_index.clone(),
        );
        self.routing_algorithm = if let Some(resolver) = &self.bike_availability {
            Arc::new(router.with_bike_availability(resolver.clone()))
        } else {
            Arc::new(router)
        };
    }

    pub fn with_ranking_policy(mut self, ranking: RankingPolicy) -> Self {
        self.ranking = ranking;
        self
    }

    pub fn with_metrics(mut self, metrics: OperationalMetrics) -> Self {
        self.operational_metrics = metrics;
        self
    }

    pub fn with_limits(mut self, limits: RoutingLimits) -> Self {
        let max_candidates = limits.max_candidates.max(1);
        let limits = RoutingLimits {
            request_timeout: limits.request_timeout,
            max_concurrent_requests: limits.max_concurrent_requests.max(1),
            max_transit_access_stops: limits.max_transit_access_stops.max(1),
            max_transit_egress_stops: limits.max_transit_egress_stops.max(1),
            max_candidates,
            max_walking_radius_metres: limits.max_walking_radius_metres.max(1),
            max_walking_seconds: limits.max_walking_seconds.max(1),
            max_bike_search_radius_metres: limits.max_bike_search_radius_metres.max(1),
            max_bike_pickup_stations: limits.max_bike_pickup_stations.max(1),
            max_bike_return_stations: limits.max_bike_return_stations.max(1),
            max_bike_connectors: limits.max_bike_connectors.min(max_candidates).max(1),
            max_bike_distance_metres: limits.max_bike_distance_metres.max(1),
            max_bike_duration_seconds: limits.max_bike_duration_seconds.max(1),
            max_bike_extra_seconds: limits.max_bike_extra_seconds,
            max_transfer_count: limits.max_transfer_count.max(1),
            max_search_expansions: limits.max_search_expansions.max(1),
            max_departures_per_query: limits.max_departures_per_query.max(1),
            max_journey_duration_seconds: limits.max_journey_duration_seconds.max(1),
        };
        self.request_slots = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_requests));
        self.limits = limits;
        self
    }

    pub async fn route(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
        let timeout = self.limits.request_timeout;
        match tokio::time::timeout(timeout, async {
            let _permit = self
                .request_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| PlanningError::TemporarilyUnavailable)?;
            self.plan_multimodal_inner(request).await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(PlanningError::TemporarilyUnavailable),
        }
    }

    async fn plan_multimodal_inner(
        &self,
        request: PlanningRequest,
    ) -> Result<PlanningOutcome, PlanningError> {
        let request_started_at = taipei_now();
        let metadata = ResultMetadata::new(
            new_request_id(),
            request_started_at,
            request
                .departure_setting
                .effective_search_time(request_started_at),
            self.transit_network.revision(),
        );
        let context = PlanningContext::new(
            &request,
            request_started_at,
            self.limits.max_journey_duration_seconds,
        )?;
        let mut generated = Vec::new();
        let mut had_transient_failure = false;
        let mut had_global_failure = false;
        let mut stage_metrics = PlannerStageMetrics::default();

        let (direct_walking, walking_transient, walking_global) =
            self.discover_direct_walking(&context).await;
        had_transient_failure |= walking_transient;
        had_global_failure |= walking_global;
        if let Some(direct_walking) = &direct_walking {
            match self
                .reconstruct_direct_walking_candidate(&request, &context, &direct_walking.path)
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

        if request.selected_modes.contains(&JourneyMode::BikeShare) {
            match access::discover_direct_bike_paths(
                self.street_routing.as_ref(),
                &self.transit_network,
                request.origin.coordinate,
                request.destination.coordinate,
                context.access_constraints(),
                context.direct_constraints(),
                &self.limits,
            )
            .await
            {
                Ok(paths) => {
                    let station_ids = paths.iter().flat_map(EndpointPath::required_station_ids);
                    let (realtime, bike_failed) = status::collect_bike_values_individually(
                        self.realtime.as_ref(),
                        station_ids,
                    )
                    .await;
                    had_transient_failure |= bike_failed;
                    for path in paths {
                        match self
                            .reconstruct_direct_bike_candidate(&request, &path, &realtime, &context)
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
                    had_transient_failure |= is_transient_provider_error(&error);
                    had_global_failure |= is_global_provider_error(&error);
                }
            }
        }

        if request.selected_modes.contains(&JourneyMode::Transit) {
            let access_started = Instant::now();
            let access = access::discover_access_points(
                self.street_routing.as_ref(),
                &self.transit_network,
                request.origin.coordinate,
                context.search_start,
                context.access_constraints(),
                &self.limits,
            )
            .await;
            let egress = access::discover_egress_points(
                self.street_routing.as_ref(),
                &self.transit_network,
                request.destination.coordinate,
                context.direct_constraints(),
                &self.limits,
            )
            .await;
            stage_metrics.candidate_discovery_duration_ms =
                access_started.elapsed().as_millis() as u64;
            let (mut access, mut egress) = match (access, egress) {
                (Ok(access), Ok(egress)) => (access, egress),
                (Err(error), _) | (_, Err(error)) => {
                    had_transient_failure |= is_transient_provider_error(&error);
                    had_global_failure |= is_global_provider_error(&error);
                    (Vec::new(), Vec::new())
                }
            };
            if request.selected_modes.contains(&JourneyMode::BikeShare)
                && !access.is_empty()
                && !egress.is_empty()
            {
                match access::discover_bike_endpoint_points(
                    self.street_routing.as_ref(),
                    &self.transit_network,
                    access::BikeEndpointRequest {
                        origin: request.origin.coordinate,
                        destination: request.destination.coordinate,
                        departure_time: context.search_start,
                        access_constraints: context.access_constraints(),
                        egress_constraints: context.direct_constraints(),
                        access: access.clone(),
                        egress: egress.clone(),
                    },
                    &self.limits,
                )
                .await
                {
                    Ok((bike_access, bike_egress)) => {
                        access = bike_access;
                        egress = bike_egress;
                    }
                    Err(error) => {
                        had_transient_failure |= is_transient_provider_error(&error);
                        had_global_failure |= is_global_provider_error(&error);
                    }
                }
            }
            stage_metrics.transit_candidate_stops = access.len() + egress.len();
            stage_metrics.transit_candidate_services = self.transit_network.services.len();

            if !access.is_empty() && !egress.is_empty() {
                let query = AlgorithmQuery {
                    origin_access: access,
                    destination_egress: egress,
                    departure_time: context.search_start,
                    latest_arrival: context.latest_arrival,
                    max_transfers: self.limits.max_transfer_count as usize,
                    max_candidates: self.limits.max_candidates,
                    max_expansions: self.limits.max_search_expansions,
                    max_journey_duration_seconds: self.limits.max_journey_duration_seconds,
                };
                let search_started = Instant::now();
                match self.routing_algorithm.route(query).await {
                    Ok(candidates) => {
                        stage_metrics.transit_realtime_lookups = candidates
                            .iter()
                            .flat_map(|candidate| &candidate.evidence)
                            .count();
                        stage_metrics.multimodal_search_duration_ms =
                            search_started.elapsed().as_millis() as u64;
                        let service_ids = candidates
                            .iter()
                            .flat_map(JourneyCandidate::transit_service_sequence);
                        let (_, condition_failed) =
                            status::collect_service_conditions(self.realtime.as_ref(), service_ids)
                                .await;
                        had_transient_failure |= condition_failed;
                        let station_ids = status::required_station_ids(&candidates);
                        let (realtime, bike_failed) =
                            status::collect_bike_values(self.realtime.as_ref(), station_ids).await;
                        had_transient_failure |= bike_failed;
                        for candidate in &candidates {
                            match self
                                .reconstruct_multimodal_candidate(
                                    &request, candidate, &realtime, &context,
                                )
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
                        if let super::algorithm::RoutingError::Departure(departure) = &error {
                            had_transient_failure |= departure.is_transient();
                        }
                        tracing::warn!(error = %error, "routing algorithm failed");
                    }
                }
            }
        }

        self.operational_metrics.record_planner_stage(stage_metrics);
        if had_global_failure {
            return Err(PlanningError::Internal);
        }
        if generated.is_empty() && had_transient_failure {
            return Err(PlanningError::TemporarilyUnavailable);
        }
        self.rank_generated(metadata, request, generated)
    }

    fn rank_generated(
        &self,
        metadata: ResultMetadata,
        request: PlanningRequest,
        generated: Vec<GeneratedCandidate>,
    ) -> Result<PlanningOutcome, PlanningError> {
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
                        serde_json::to_value(&request.selected_modes).unwrap_or_default(),
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
}

impl PlanningContext {
    pub(crate) fn new(
        request: &PlanningRequest,
        request_started_at: OffsetDateTime,
        max_journey_duration_seconds: u32,
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
        })
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

pub(crate) struct GeneratedCandidate {
    pub(crate) ranking: CandidateJourney,
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
