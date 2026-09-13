use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use futures_util::{StreamExt, stream};
use time::{Duration, OffsetDateTime};

use crate::{
    api::{PlanningRequest, RequestCoordinate, new_request_id},
    domain::ranking::{CandidateJourney, RankingPolicy},
    domain::transit::{TransitItinerary, TransitNetwork},
    domain::{
        ids::BikeShareStationId,
        journey::{
            EmptyResult, JourneyMode, JourneyOption, OptionsResult, PlanningError, PlanningOutcome,
            ResultMetadata, SemanticObject,
        },
        normalize_to_taipei,
        realtime::FreshnessState,
    },
    observability::{OperationalMetrics, PlannerStageMetrics},
    providers::realtime::{ArrivalQuery, NormalizedRealtimeValue, RealtimeProvider, RealtimeValue},
    street::{
        Coordinate, DetailedPath, DetailedPathQuery, MatrixCell, MatrixReachability, ProviderError,
        RoutingConstraints, RoutingTime, RoutingTimeKind, StreetRoutingSession, StreetTravelMode,
    },
};

use super::reconstruct::{
    apply_ranked_journey, assign_result_local_ids, taipei_now, to_street_coordinate,
};
use super::{bike, discover, enrich, graph, graph::StreetMetric, search};

#[derive(Clone)]
pub struct RoutingService {
    pub(crate) street_routing: StreetRoutingSession,
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
    pub max_bike_share_stations: usize,
    pub max_candidates: usize,
    pub max_candidate_stops: usize,
    pub max_candidate_states: usize,
    pub max_services_per_stop: usize,
    pub max_candidate_services: usize,
    pub max_transit_itineraries: usize,
    pub max_destination_side_stops: usize,
    pub max_origin_side_stops: usize,
    pub max_topology_stops_visited: usize,
    pub max_topology_search_states: usize,
    pub max_geographic_detour_metres: u32,
    pub max_walking_radius_metres: u32,
    pub max_realtime_concurrency: usize,
    pub max_walking_seconds: u32,
    pub min_walking_before_bike_seconds: u32,
    pub max_bike_search_radius_metres: u32,
    pub max_bike_pickup_stations: usize,
    pub max_bike_return_stations: usize,
    pub max_bike_connectors: usize,
    pub max_bike_distance_metres: u32,
    pub max_bike_duration_seconds: u32,
    pub max_bike_extra_seconds: u32,
    pub max_labels_per_node: usize,
    pub max_transfer_count: u32,
    pub max_bike_share_expansions: usize,
    pub max_search_expansions: usize,
    pub max_journey_duration_seconds: u32,
    pub max_final_pareto_journeys: usize,
}

impl Default for RoutingLimits {
    fn default() -> Self {
        Self {
            request_timeout: std::time::Duration::from_secs(5),
            max_concurrent_requests: 32,
            max_transit_access_stops: 24,
            max_bike_share_stations: 12,
            max_candidates: 16,
            max_candidate_stops: 24,
            max_candidate_states: 256,
            max_services_per_stop: 8,
            max_candidate_services: 32,
            max_transit_itineraries: 64,
            max_destination_side_stops: 24,
            max_origin_side_stops: 24,
            max_topology_stops_visited: 64,
            max_topology_search_states: 256,
            max_geographic_detour_metres: 10_000,
            max_walking_radius_metres: 2_000,
            max_realtime_concurrency: 8,
            max_walking_seconds: 30 * 60,
            min_walking_before_bike_seconds: 4 * 60,
            max_bike_search_radius_metres: 2_000,
            max_bike_pickup_stations: 6,
            max_bike_return_stations: 6,
            max_bike_connectors: 24,
            max_bike_distance_metres: 20_000,
            max_bike_duration_seconds: 2 * 60 * 60,
            max_bike_extra_seconds: 5 * 60,
            max_labels_per_node: 16,
            max_transfer_count: 4,
            max_bike_share_expansions: 1,
            max_search_expansions: 2_000,
            max_journey_duration_seconds: 4 * 60 * 60,
            max_final_pareto_journeys: 16,
        }
    }
}

impl RoutingService {
    pub fn new(street_routing: StreetRoutingSession, realtime: Arc<dyn RealtimeProvider>) -> Self {
        Self {
            street_routing,
            realtime,
            transit_network: Arc::new(TransitNetwork::default()),
            ranking: RankingPolicy::default(),
            request_slots: Arc::new(tokio::sync::Semaphore::new(
                RoutingLimits::default().max_concurrent_requests,
            )),
            operational_metrics: OperationalMetrics::default(),
            limits: RoutingLimits::default(),
        }
    }

    pub fn with_transit_network(mut self, transit_network: TransitNetwork) -> Self {
        self.transit_network = Arc::new(transit_network);
        self
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
        let max_transit_access_stops = limits.max_transit_access_stops.max(1);
        let max_bike_share_stations = limits.max_bike_share_stations.max(1);
        let max_candidates = limits.max_candidates.max(1);
        let limits = RoutingLimits {
            max_concurrent_requests: limits.max_concurrent_requests.max(1),
            max_transit_access_stops,
            max_bike_share_stations,
            max_candidates,
            max_candidate_stops: limits
                .max_candidate_stops
                .min(max_transit_access_stops)
                .clamp(1, 32),
            max_candidate_states: limits.max_candidate_states.max(1),
            max_services_per_stop: limits.max_services_per_stop.max(1),
            max_candidate_services: limits.max_candidate_services.max(1),
            max_transit_itineraries: limits.max_transit_itineraries.max(1),
            max_destination_side_stops: limits.max_destination_side_stops.max(1),
            max_origin_side_stops: limits.max_origin_side_stops.max(1),
            max_topology_stops_visited: limits.max_topology_stops_visited.max(1),
            max_topology_search_states: limits.max_topology_search_states.max(1),
            max_geographic_detour_metres: limits.max_geographic_detour_metres.max(1),
            max_walking_radius_metres: limits.max_walking_radius_metres.max(1),
            max_realtime_concurrency: limits.max_realtime_concurrency.clamp(1, 8),
            max_walking_seconds: limits.max_walking_seconds.max(1),
            min_walking_before_bike_seconds: limits.min_walking_before_bike_seconds.max(1),
            max_bike_search_radius_metres: limits.max_bike_search_radius_metres.max(1),
            max_bike_pickup_stations: limits
                .max_bike_pickup_stations
                .min(max_bike_share_stations)
                .max(1),
            max_bike_return_stations: limits
                .max_bike_return_stations
                .min(max_bike_share_stations)
                .max(1),
            max_bike_connectors: limits.max_bike_connectors.min(max_candidates).max(1),
            max_bike_distance_metres: limits.max_bike_distance_metres.max(1),
            max_bike_duration_seconds: limits.max_bike_duration_seconds.max(1),
            max_bike_extra_seconds: limits.max_bike_extra_seconds,
            max_labels_per_node: limits.max_labels_per_node.max(1),
            max_transfer_count: limits.max_transfer_count.max(1),
            max_bike_share_expansions: limits.max_bike_share_expansions.max(1),
            max_search_expansions: limits.max_search_expansions.max(1),
            max_journey_duration_seconds: limits.max_journey_duration_seconds.max(1),
            max_final_pareto_journeys: limits.max_final_pareto_journeys.min(max_candidates).max(1),
            ..limits
        };
        self.request_slots = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_requests));
        self.limits = limits;
        self
    }

    pub async fn route(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
        let request_timeout = self.limits.request_timeout;
        match tokio::time::timeout(request_timeout, async {
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
}

fn bike_availability_map(
    realtime: &enrich::RealtimeContext,
) -> HashMap<BikeShareStationId, crate::domain::realtime::BikeAvailability> {
    realtime
        .station_ids()
        .filter_map(|station_id| {
            let status = realtime.bike(station_id)?;
            let RealtimeValue::BikeAvailability(availability) = &status.value else {
                return None;
            };
            (status.freshness.state == FreshnessState::Fresh)
                .then(|| (station_id.clone(), availability.clone()))
        })
        .collect()
}

impl RoutingService {
    async fn plan_multimodal_inner(
        &self,
        request: PlanningRequest,
    ) -> Result<PlanningOutcome, PlanningError> {
        let request_started_at = taipei_now();
        let metadata = ResultMetadata::new(
            new_request_id(),
            taipei_now(),
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

        // Discover direct walking as a normal optional graph edge. It is the
        // only route that does not need transit topology or timing.
        let (direct_walking, walking_transient, walking_global) =
            self.discover_direct_walking(&context).await;
        had_transient_failure |= walking_transient;
        had_global_failure |= walking_global;

        let mut transit_candidate = discover::DiscoveryResult::default();
        let mut transit_timing = enrich::TransitTiming::default();
        let mut realtime = enrich::RealtimeContext::default();
        if request.selected_modes.contains(&JourneyMode::Transit) {
            let discovery_started = Instant::now();
            match discover::Discovery::new(&self.street_routing, &self.transit_network)
                .discover(&request, &context, &self.limits)
                .await
            {
                Ok(candidate) => {
                    transit_candidate = candidate;
                    stage_metrics.transit_candidate_stops = transit_candidate.boarding_points.len();
                    stage_metrics.transit_candidate_services = transit_candidate.services.len();
                    if !transit_candidate.is_empty() {
                        let static_itineraries = transit_candidate
                            .topology_itineraries
                            .iter()
                            .map(|topology| crate::domain::transit::TransitItinerary {
                                service_day: topology.service_day,
                                access_departure_at: context.search_start,
                                arrival_at: topology
                                    .rides
                                    .last()
                                    .map(|ride| ride.scheduled_arrival)
                                    .unwrap_or(context.search_start),
                                rides: topology.rides.clone(),
                            })
                            .collect::<Vec<_>>();
                        let timing_started = Instant::now();
                        let (collected, realtime_failed) = self
                            .collect_request_realtime(&transit_candidate, &static_itineraries)
                            .await;
                        stage_metrics.transit_realtime_lookups =
                            transit_realtime_lookup_count(&transit_candidate, &static_itineraries);
                        realtime = collected;
                        had_transient_failure |= realtime_failed;
                        transit_timing = if transit_candidate.topology_itineraries.is_empty() {
                            enrich::resolve_timing(
                                &self.transit_network,
                                &transit_candidate,
                                &static_itineraries,
                                &realtime,
                                &context,
                            )
                            .await
                        } else {
                            enrich::resolve_topology_timing(
                                &self.transit_network,
                                &transit_candidate,
                                &transit_candidate.topology_itineraries,
                                &realtime,
                                &context,
                            )
                            .await
                        };
                        stage_metrics.realtime_resolution_duration_ms =
                            timing_started.elapsed().as_millis() as u64;
                    }
                }
                Err(error) => {
                    had_transient_failure |= is_transient_provider_error(&error);
                    had_global_failure |= is_global_provider_error(&error);
                    tracing::warn!(error = %error, "transit candidate discovery failed");
                }
            }
            stage_metrics.candidate_discovery_duration_ms =
                discovery_started.elapsed().as_millis() as u64;
        }

        // BikeShare expansion consumes the complete walking base graph, not a
        // list of transit-specific gaps. A direct edge exists only when the
        // street provider produced a reachable path.
        let mut base_candidate = transit_candidate;
        base_candidate.direct_walking = direct_walking.as_ref().map(|edge| edge.metric);
        let mut route_graph = graph::from_discovery(&base_candidate);
        graph::add_timed_transit(&mut route_graph, &self.transit_network, &transit_timing);

        if request.selected_modes.contains(&JourneyMode::BikeShare) {
            match bike::BikeShareOpportunityDiscovery::new(
                &self.street_routing,
                &self.transit_network,
            )
            .discover(&request, &context, &self.limits, &route_graph)
            .await
            {
                Ok(expansion) => {
                    stage_metrics.bike_opportunities = expansion.opportunities.len();
                    stage_metrics.bike_stations_queried = expansion.station_ids.len();
                    let (bike_values, bike_failed) =
                        self.collect_bike_realtime_v2(&expansion.station_ids).await;
                    had_transient_failure |= bike_failed;
                    realtime.extend(bike_values);
                    let availability = bike_availability_map(&realtime);
                    graph::add_bike_alternatives(
                        &mut route_graph,
                        &expansion.opportunities,
                        &availability,
                    );
                }
                Err(error) => {
                    had_transient_failure |= is_transient_provider_error(&error);
                    had_global_failure |= is_global_provider_error(&error);
                    tracing::warn!(error = %error, "BikeShare expansion failed");
                }
            }
        }

        stage_metrics.multimodal_graph_nodes = route_graph.nodes.len();
        stage_metrics.multimodal_graph_edges = route_graph.edges.len();
        let search_started = Instant::now();
        let search = search::search(
            &route_graph,
            context.search_start,
            context.latest_arrival,
            &self.search_limits(),
        );
        stage_metrics.search_expansions += search.stats.expansions;
        stage_metrics.labels_retained += search.stats.labels_retained;
        stage_metrics.labels_pruned += search.stats.labels_dominated_or_pruned;
        stage_metrics.multimodal_search_duration_ms += search_started.elapsed().as_millis() as u64;
        for journey in search.journeys {
            match self
                .reconstruct_multimodal_candidate(
                    &request,
                    &route_graph,
                    &journey,
                    &realtime,
                    &context,
                    direct_walking.as_ref().map(|edge| &edge.path),
                )
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

        self.operational_metrics.record_planner_stage(stage_metrics);
        if had_global_failure {
            return Err(PlanningError::Internal);
        }
        if generated.is_empty() && had_transient_failure {
            return Err(PlanningError::TemporarilyUnavailable);
        }
        self.rank_generated(metadata, request, generated)
    }

    fn search_limits(&self) -> search::SearchLimits {
        search::SearchLimits {
            max_labels_per_node: self.limits.max_labels_per_node,
            max_transfer_count: self.limits.max_transfer_count,
            max_bike_share_expansions: self.limits.max_bike_share_expansions,
            max_search_expansions: self.limits.max_search_expansions,
            max_journey_duration_seconds: self.limits.max_journey_duration_seconds,
            max_final_pareto_journeys: self.limits.max_final_pareto_journeys,
        }
    }

    fn rank_generated(
        &self,
        metadata: ResultMetadata,
        request: PlanningRequest,
        generated: Vec<GeneratedCandidate>,
    ) -> Result<PlanningOutcome, PlanningError> {
        let schedule_only_keys = generated
            .iter()
            .filter(|candidate| candidate.schedule_only)
            .map(|candidate| candidate.ranking.stable_key.clone())
            .collect::<HashSet<_>>();
        let ranked = self.ranking.rank_with_schedule_only(
            generated
                .iter()
                .map(|candidate| candidate.ranking.clone())
                .collect(),
            &schedule_only_keys,
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
                    parameters: BTreeMap::from([(
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

    async fn collect_request_realtime(
        &self,
        candidate: &discover::DiscoveryResult,
        itineraries: &[TransitItinerary],
    ) -> (enrich::RealtimeContext, bool) {
        let mut pairs = HashSet::new();
        let mut service_ids = HashSet::new();
        for itinerary in itineraries.iter().take(self.limits.max_transit_itineraries) {
            for ride in &itinerary.rides {
                service_ids.insert(ride.service_id.clone());
                if enrich::requires_realtime_arrival(ride) {
                    pairs.insert((ride.service_id.clone(), ride.board_stop_id.clone()));
                }
            }
        }
        for itinerary in candidate
            .topology_itineraries
            .iter()
            .take(self.limits.max_transit_itineraries)
        {
            for ride in &itinerary.rides {
                service_ids.insert(ride.service_id.clone());
                if enrich::requires_realtime_arrival(ride) {
                    pairs.insert((ride.service_id.clone(), ride.board_stop_id.clone()));
                }
            }
        }
        let mut values = Vec::new();
        let mut had_transient_failure = false;
        let mut pairs = pairs.into_iter().collect::<Vec<_>>();
        pairs.sort_by(|left, right| {
            left.0
                .as_str()
                .cmp(right.0.as_str())
                .then(left.1.as_str().cmp(right.1.as_str()))
        });
        let realtime = self.realtime.clone();
        let mut lookups = stream::iter(pairs.iter().cloned().map(|pair| {
            let realtime = realtime.clone();
            async move {
                let result = realtime
                    .arrival_estimates(ArrivalQuery::for_ride(pair.0.clone(), pair.1.clone()))
                    .await;
                (pair, result)
            }
        }))
        .buffer_unordered(self.limits.max_realtime_concurrency.max(1))
        .collect::<Vec<_>>()
        .await;
        lookups.sort_by(|left, right| {
            left.0
                .0
                .as_str()
                .cmp(right.0.0.as_str())
                .then(left.0.1.as_str().cmp(right.0.1.as_str()))
        });
        for (pair, result) in lookups {
            match result {
                Ok(arrivals) => values.extend(arrivals),
                Err(error) => {
                    had_transient_failure |= error.is_transient();
                    tracing::warn!(
                        service_id = pair.0.as_str(),
                        boarding_point_id = pair.1.as_str(),
                        error = %error,
                        "realtime arrival enrichment failed"
                    );
                }
            }
        }
        if !service_ids.is_empty() {
            let mut ids = service_ids.into_iter().collect::<Vec<_>>();
            ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            match self.realtime.service_conditions(&ids).await {
                Ok(conditions) => values.extend(conditions),
                Err(error) => {
                    had_transient_failure |= error.is_transient();
                    tracing::warn!(error = %error, "realtime service-condition enrichment failed");
                }
            }
        }
        (
            enrich::RealtimeContext::from_values(values),
            had_transient_failure,
        )
    }

    async fn collect_bike_realtime_v2(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> (Vec<NormalizedRealtimeValue>, bool) {
        if station_ids.is_empty() {
            return (Vec::new(), false);
        }
        match self.realtime.bike_availability(station_ids).await {
            Ok(values) => (values, false),
            Err(error) => {
                tracing::warn!(error = %error, "realtime BikeShare enrichment failed");
                (Vec::new(), error.is_transient())
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct PlanningContext {
    pub(crate) request: PlanningRequest,
    pub(crate) effective_search_time: OffsetDateTime,
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
            effective_search_time,
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

pub(crate) fn planner_coordinate(
    coordinate: RequestCoordinate,
) -> Result<Coordinate, ProviderError> {
    Coordinate::try_new(coordinate.latitude, coordinate.longitude)
}

pub(crate) fn street_metric(cell: &MatrixCell) -> Option<StreetMetric> {
    match cell.reachability {
        MatrixReachability::Reachable { duration, distance } => Some(StreetMetric {
            duration_seconds: duration.0,
            distance_metres: distance.0,
        }),
        MatrixReachability::Unreachable => None,
    }
}

#[derive(Clone)]
pub(crate) struct GeneratedCandidate {
    pub(crate) ranking: CandidateJourney,
    pub(crate) option: JourneyOption,
    pub(crate) schedule_only: bool,
}

enum PathAttempt {
    Found(DetailedPath),
    Unreachable,
    Failed(ProviderError),
}

impl PathAttempt {
    fn was_transient_failure(&self) -> bool {
        matches!(
            self,
            Self::Failed(
                ProviderError::Timeout
                    | ProviderError::RateLimited
                    | ProviderError::TemporarilyUnavailable
            )
        )
    }

    fn is_global_failure(&self) -> bool {
        matches!(
            self,
            Self::Failed(
                ProviderError::InvalidRequest { .. }
                    | ProviderError::MalformedResponse
                    | ProviderError::UnsupportedCapability
                    | ProviderError::InconsistentGraphRevision { .. }
                    | ProviderError::ResponseTooLarge { .. }
            )
        )
    }
}

pub(crate) fn is_transient_provider_error(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::Timeout | ProviderError::RateLimited | ProviderError::TemporarilyUnavailable
    )
}

fn is_global_provider_error(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::InvalidRequest { .. }
            | ProviderError::MalformedResponse
            | ProviderError::UnsupportedCapability
            | ProviderError::InconsistentGraphRevision { .. }
            | ProviderError::ResponseTooLarge { .. }
    )
}

fn transit_realtime_lookup_count(
    candidate: &discover::DiscoveryResult,
    itineraries: &[TransitItinerary],
) -> usize {
    itineraries
        .iter()
        .flat_map(|itinerary| &itinerary.rides)
        .chain(
            candidate
                .topology_itineraries
                .iter()
                .flat_map(|itinerary| &itinerary.rides),
        )
        .filter(|ride| enrich::requires_realtime_arrival(ride))
        .map(|ride| (ride.service_id.clone(), ride.board_stop_id.clone()))
        .collect::<HashSet<_>>()
        .len()
}

pub(crate) struct DirectWalking {
    pub(crate) metric: StreetMetric,
    pub(crate) path: DetailedPath,
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
            .detailed_path(DetailedPathQuery {
                travel_mode: mode,
                origin,
                destination,
                constraints,
            })
            .await
        {
            Ok(path) => PathAttempt::Found(path),
            Err(ProviderError::UnreachableEndpoints) => PathAttempt::Unreachable,
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
        let failed = attempt.was_transient_failure();
        let global_failure = attempt.is_global_failure();
        let PathAttempt::Found(path) = attempt else {
            return (None, failed, global_failure);
        };
        let metric = StreetMetric {
            duration_seconds: path.duration.0,
            distance_metres: path.distance.0,
        };
        (Some(DirectWalking { metric, path }), false, false)
    }
}

impl From<ProviderError> for PlanningError {
    fn from(error: ProviderError) -> Self {
        match error {
            ProviderError::Timeout
            | ProviderError::RateLimited
            | ProviderError::TemporarilyUnavailable => Self::TemporarilyUnavailable,
            ProviderError::UnreachableEndpoints => unreachable_is_handled_before_conversion(),
            ProviderError::InvalidRequest { .. }
            | ProviderError::MalformedResponse
            | ProviderError::UnsupportedCapability
            | ProviderError::InconsistentGraphRevision { .. }
            | ProviderError::ResponseTooLarge { .. } => Self::Internal,
        }
    }
}

fn unreachable_is_handled_before_conversion() -> ! {
    unreachable!("unreachable paths are converted to successful empty results")
}
