use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use futures_util::{StreamExt, stream};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::{
    api::{JourneyMode, PlanningRequest, RequestCoordinate, new_request_id, normalize_to_taipei},
    live_status::{FreshnessReason, FreshnessState},
    normalization::BikeShareStationId,
    observability::{OperationalMetrics, PlannerStageMetrics},
    ranking::{
        BikeActionEvidence, CandidateJourney, JourneyClass, JourneyLabel, RankingPolicy,
        RankingReason, TransferEvidence, TransferKind,
    },
    realtime::{ArrivalQuery, NormalizedRealtimeValue, RealtimeProvider, RealtimeValue},
    street::{
        Attribution, Coordinate, DetailedPath, DetailedPathQuery, Maneuver, ManeuverKind,
        MatrixCell, MatrixReachability, ProviderError, RoutingConstraints, RoutingTime,
        RoutingTimeKind, StreetRoutingSession, StreetTravelMode,
    },
    transit::{TransitItinerary, TransitMode, TransitNetwork, TransitRide},
};

pub mod bike_share_expansion;
pub mod candidate_discovery;
pub mod multimodal_graph;
pub mod search;
pub mod transit_timing;

#[derive(Clone)]
pub struct JourneyPlanningService {
    street_routing: StreetRoutingSession,
    realtime: Arc<dyn RealtimeProvider>,
    transit_network: Arc<TransitNetwork>,
    ranking: RankingPolicy,
    limits: PlanningLimits,
    request_slots: Arc<tokio::sync::Semaphore>,
    operational_metrics: OperationalMetrics,
}

#[derive(Clone, Debug)]
pub struct PlanningLimits {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreetMetric {
    pub duration_seconds: u32,
    pub distance_metres: u32,
}

impl Default for PlanningLimits {
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

impl JourneyPlanningService {
    pub fn new(street_routing: StreetRoutingSession, realtime: Arc<dyn RealtimeProvider>) -> Self {
        Self {
            street_routing,
            realtime,
            transit_network: Arc::new(TransitNetwork::default()),
            ranking: RankingPolicy::default(),
            request_slots: Arc::new(tokio::sync::Semaphore::new(
                PlanningLimits::default().max_concurrent_requests,
            )),
            operational_metrics: OperationalMetrics::default(),
            limits: PlanningLimits::default(),
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

    pub fn with_limits(mut self, limits: PlanningLimits) -> Self {
        let max_transit_access_stops = limits.max_transit_access_stops.max(1);
        let max_bike_share_stations = limits.max_bike_share_stations.max(1);
        let max_candidates = limits.max_candidates.max(1);
        let limits = PlanningLimits {
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

    pub async fn plan(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
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
    realtime: &transit_timing::RealtimeContext,
) -> HashMap<BikeShareStationId, crate::normalization::BikeAvailability> {
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

impl JourneyPlanningService {
    async fn plan_multimodal_inner(
        &self,
        request: PlanningRequest,
    ) -> Result<PlanningOutcome, PlanningError> {
        let request_started_at = taipei_now();
        let metadata = ResultMetadata::new(
            new_request_id(),
            taipei_now(),
            request_started_at,
            &self.transit_network,
            &request,
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

        // Direct walking remains the unconditional fallback. It is also the
        // only route that does not need a transit backbone.
        let (walking, walking_transient, walking_global) = self.generate_walking(&context).await;
        let direct_walking = walking
            .iter()
            .find(|candidate| candidate.option.travel_modes == [JourneyMode::Walking])
            .map(|candidate| StreetMetric {
                duration_seconds: candidate.option.duration_seconds,
                distance_metres: candidate
                    .option
                    .legs
                    .first()
                    .map(|leg| leg.distance_metres)
                    .unwrap_or_default(),
            });
        generated.extend(walking);
        had_transient_failure |= walking_transient;
        had_global_failure |= walking_global;

        let mut transit_candidate = candidate_discovery::TransitCandidateNetwork::default();
        let mut timed_backbone = transit_timing::TimedTransitBackbone::default();
        let mut realtime = transit_timing::RealtimeContext::default();
        if request.selected_modes.contains(&JourneyMode::Transit) {
            let discovery_started = Instant::now();
            match candidate_discovery::TransitCandidateDiscovery::new(
                &self.street_routing,
                &self.transit_network,
            )
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
                            .map(|topology| crate::transit::TransitItinerary {
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
                        timed_backbone = if transit_candidate.topology_itineraries.is_empty() {
                            transit_timing::resolve_backbone(
                                &self.transit_network,
                                &transit_candidate,
                                &static_itineraries,
                                &realtime,
                                &context,
                            )
                            .await
                        } else {
                            transit_timing::resolve_topology_backbone(
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
        // list of transit-specific gaps. Keep a conservative straight-line
        // fallback for bike-only requests whose injected street provider does
        // not expose a direct path; a reachable direct path always wins.
        let mut base_candidate = transit_candidate;
        base_candidate.direct_walking = direct_walking.or_else(|| {
            (request.selected_modes.contains(&JourneyMode::BikeShare)
                && base_candidate.topology_itineraries.is_empty())
            .then(|| StreetMetric {
                distance_metres: approximate_distance_metres(
                    request.origin.coordinate,
                    request.destination.coordinate,
                ),
                duration_seconds: approximate_distance_metres(
                    request.origin.coordinate,
                    request.destination.coordinate,
                )
                .saturating_mul(10)
                .saturating_div(13),
            })
        });
        let base_graph = multimodal_graph::build_base_graph(&base_candidate);

        if request.selected_modes.contains(&JourneyMode::BikeShare) {
            match bike_share_expansion::BikeShareOpportunityDiscovery::new(
                &self.street_routing,
                &self.transit_network,
            )
            .discover(&request, &context, &self.limits, &base_graph)
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
                    let graph = multimodal_graph::build_graph(
                        &self.transit_network,
                        &base_candidate,
                        &timed_backbone,
                        &expansion.opportunities,
                        &availability,
                    );
                    stage_metrics.multimodal_graph_nodes = graph.nodes.len();
                    stage_metrics.multimodal_graph_edges = graph.edges.len();
                    let search_started = Instant::now();
                    let search = search::search(
                        &graph,
                        context.search_start,
                        context.latest_arrival,
                        &self.search_limits(),
                    );
                    stage_metrics.search_expansions += search.stats.expansions;
                    stage_metrics.labels_retained += search.stats.labels_retained;
                    stage_metrics.labels_pruned += search.stats.labels_dominated_or_pruned;
                    stage_metrics.multimodal_search_duration_ms +=
                        search_started.elapsed().as_millis() as u64;
                    for journey in search.journeys {
                        match self
                            .reconstruct_multimodal_candidate(
                                &request, &graph, &journey, &realtime, &context,
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
                    had_transient_failure |= is_transient_provider_error(&error);
                    had_global_failure |= is_global_provider_error(&error);
                    tracing::warn!(error = %error, "BikeShare expansion failed");
                }
            }
        } else if request.selected_modes.contains(&JourneyMode::Transit)
            && !timed_backbone.itineraries.is_empty()
        {
            let graph = multimodal_graph::build_graph(
                &self.transit_network,
                &base_candidate,
                &timed_backbone,
                &[],
                &HashMap::new(),
            );
            stage_metrics.multimodal_graph_nodes = graph.nodes.len();
            stage_metrics.multimodal_graph_edges = graph.edges.len();
            let search_started = Instant::now();
            let search = search::search(
                &graph,
                context.search_start,
                context.latest_arrival,
                &self.search_limits(),
            );
            stage_metrics.search_expansions += search.stats.expansions;
            stage_metrics.labels_retained += search.stats.labels_retained;
            stage_metrics.labels_pruned += search.stats.labels_dominated_or_pruned;
            stage_metrics.multimodal_search_duration_ms +=
                search_started.elapsed().as_millis() as u64;
            for journey in search.journeys {
                match self
                    .reconstruct_multimodal_candidate(
                        &request, &graph, &journey, &realtime, &context,
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
        candidate: &candidate_discovery::TransitCandidateNetwork,
        itineraries: &[TransitItinerary],
    ) -> (transit_timing::RealtimeContext, bool) {
        let mut pairs = HashSet::new();
        let mut service_ids = HashSet::new();
        for itinerary in itineraries.iter().take(self.limits.max_transit_itineraries) {
            for ride in &itinerary.rides {
                service_ids.insert(ride.service_id.clone());
                if transit_timing::requires_realtime_arrival(ride) {
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
                if transit_timing::requires_realtime_arrival(ride) {
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
            transit_timing::RealtimeContext::from_values(values),
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

    async fn reconstruct_multimodal_candidate(
        &self,
        request: &PlanningRequest,
        graph: &multimodal_graph::MultimodalGraph,
        journey: &search::SearchLabel,
        realtime: &transit_timing::RealtimeContext,
        context: &PlanningContext,
    ) -> Result<Option<GeneratedCandidate>, PlanningError> {
        let mut legs = Vec::new();
        let mut transfers = Vec::new();
        let mut leg_windows = Vec::<(String, OffsetDateTime, OffsetDateTime)>::new();
        let mut pending_transfers = Vec::<(usize, u32)>::new();
        let mut current_time = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { time } => {
                let deadline = normalize_to_taipei(time);
                let Some(departure) = latest_departure_for_path(graph, &journey.edge_ids, deadline)
                else {
                    return Ok(None);
                };
                departure
            }
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => journey.departure_time,
        };
        let departure_time = current_time;
        let mut live_evidence = Vec::new();
        let mut required_states = Vec::new();
        let mut schedule_only = journey.schedule_only;
        let mut travel_modes = Vec::new();
        let mut leg_transit_modes = Vec::<Option<TransitMode>>::new();
        let mut main_service_sequence = Vec::new();
        let mut transfer_evidence = Vec::new();
        let mut bike_availability = None;
        let mut walking_seconds = 0_u32;

        for edge_id in &journey.edge_ids {
            let Some(edge) = graph.edges.get(*edge_id) else {
                return Err(PlanningError::Internal);
            };
            match edge {
                multimodal_graph::CandidateEdge::Walking { from, to, .. } => {
                    let Some(path) = self
                        .hydrate_street_edge(
                            request,
                            from,
                            to,
                            StreetTravelMode::Walking,
                            context.constraints(current_time, RoutingTimeKind::DepartAt),
                        )
                        .await?
                    else {
                        return Ok(None);
                    };
                    let Some(from_place) = self.node_place(request, from) else {
                        return Ok(None);
                    };
                    let Some(to_place) = self.node_place(request, to) else {
                        return Ok(None);
                    };
                    let departure = current_time;
                    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
                    let leg_id = format!("candidate_leg_{}", legs.len());
                    legs.push(walking_leg(
                        leg_id.clone(),
                        &from_place,
                        &to_place,
                        departure,
                        arrival,
                        &path,
                    ));
                    leg_transit_modes.push(None);
                    leg_windows.push((leg_id, departure, arrival));
                    for (from_index, duration_seconds) in pending_transfers.drain(..) {
                        if let Some((from_id, _, from_arrival)) = leg_windows.get(from_index)
                            && let Some((to_id, to_departure, _)) = leg_windows.last()
                        {
                            let available_seconds =
                                seconds_between(*from_arrival, *to_departure).unwrap_or(0);
                            let kind = transfer_kind(
                                leg_transit_modes.get(from_index).copied().flatten(),
                                leg_transit_modes.last().copied().flatten(),
                            );
                            transfer_evidence.push(TransferEvidence {
                                kind,
                                physical_seconds: duration_seconds,
                                available_seconds,
                            });
                            transfers.push(Transfer {
                                from_leg_id: from_id.clone(),
                                to_leg_id: to_id.clone(),
                                connector_leg_ids: Vec::new(),
                                duration_seconds,
                                available_seconds,
                                class: if available_seconds >= duration_seconds {
                                    JourneyClass::Reliable
                                } else {
                                    JourneyClass::Feasible
                                },
                                reasons: if available_seconds >= duration_seconds {
                                    Vec::new()
                                } else {
                                    vec![RankingReason::TightTransferMargin]
                                },
                                geometry: None,
                                guidance: Vec::new(),
                                warnings: Vec::new(),
                            });
                        }
                    }
                    current_time = arrival;
                    walking_seconds = walking_seconds.saturating_add(path.duration.0);
                    push_unique_mode(&mut travel_modes, JourneyMode::Walking);
                }
                multimodal_graph::CandidateEdge::Transit { ride, .. } => {
                    let departure = ride.departure;
                    if current_time > departure {
                        return Ok(None);
                    }
                    let service = self
                        .transit_network
                        .service(&ride.ride.service_id)
                        .ok_or(PlanningError::Internal)?;
                    let boarding = self
                        .transit_network
                        .stop(&ride.ride.board_stop_id)
                        .ok_or(PlanningError::Internal)?;
                    let alighting = self
                        .transit_network
                        .stop(&ride.ride.alight_stop_id)
                        .ok_or(PlanningError::Internal)?;
                    let evidence = ride
                        .evidence
                        .iter()
                        .map(|status| {
                            let (kind, observed_at) = match &status.value {
                                RealtimeValue::ArrivalEstimate(arrival) => (
                                    LiveStatusKind::ArrivalEstimate,
                                    Some(arrival.transmitted_at),
                                ),
                                RealtimeValue::ServiceCondition(_) => (
                                    LiveStatusKind::ServiceCondition,
                                    status.provider_published_at,
                                ),
                                RealtimeValue::BikeAvailability(availability) => (
                                    LiveStatusKind::BikeAvailability,
                                    bike_observed_at(availability),
                                ),
                            };
                            status_evidence(status, kind, EvidenceType::Live, observed_at)
                        })
                        .collect::<Vec<_>>();
                    required_states.extend(ride.required_states.iter().copied());
                    live_evidence.extend(evidence.clone());
                    schedule_only |= ride.schedule_only;
                    let effective = EffectiveRide {
                        ride: ride.ride.clone(),
                        departure,
                        arrival: ride.arrival,
                        evidence,
                    };
                    let leg_id = format!("candidate_leg_{}", legs.len());
                    legs.push(transit_leg(
                        leg_id.clone(),
                        service,
                        boarding,
                        alighting,
                        &effective,
                    ));
                    leg_transit_modes.push(Some(service.transit_mode));
                    leg_windows.push((leg_id, departure, ride.arrival));
                    for (from_index, duration_seconds) in pending_transfers.drain(..) {
                        if let Some((from_id, _, from_arrival)) = leg_windows.get(from_index)
                            && let Some((to_id, to_departure, _)) = leg_windows.last()
                        {
                            let available_seconds =
                                seconds_between(*from_arrival, *to_departure).unwrap_or(0);
                            let kind = transfer_kind(
                                leg_transit_modes.get(from_index).copied().flatten(),
                                leg_transit_modes.last().copied().flatten(),
                            );
                            transfer_evidence.push(TransferEvidence {
                                kind,
                                physical_seconds: duration_seconds,
                                available_seconds,
                            });
                            transfers.push(Transfer {
                                from_leg_id: from_id.clone(),
                                to_leg_id: to_id.clone(),
                                connector_leg_ids: Vec::new(),
                                duration_seconds,
                                available_seconds,
                                class: if available_seconds >= duration_seconds {
                                    JourneyClass::Reliable
                                } else {
                                    JourneyClass::Feasible
                                },
                                reasons: if available_seconds >= duration_seconds {
                                    Vec::new()
                                } else {
                                    vec![RankingReason::TightTransferMargin]
                                },
                                geometry: None,
                                guidance: Vec::new(),
                                warnings: Vec::new(),
                            });
                        }
                    }
                    current_time = ride.arrival;
                    push_unique_mode(&mut travel_modes, JourneyMode::Transit);
                    main_service_sequence.push(service.id.as_str().to_owned());
                }
                multimodal_graph::CandidateEdge::BikeShare {
                    from,
                    to,
                    pickup_station_id,
                    return_station_id,
                    metric: _,
                    availability,
                } => {
                    let Some(path) = self
                        .hydrate_street_edge(
                            request,
                            from,
                            to,
                            StreetTravelMode::Bicycle,
                            context.constraints(current_time, RoutingTimeKind::DepartAt),
                        )
                        .await?
                    else {
                        return Ok(None);
                    };
                    let departure = current_time;
                    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
                    let pickup = self
                        .transit_network
                        .bike_share_station(pickup_station_id)
                        .ok_or(PlanningError::Internal)?;
                    let return_station = self
                        .transit_network
                        .bike_share_station(return_station_id)
                        .ok_or(PlanningError::Internal)?;
                    let pickup_status = realtime.bike(pickup_station_id);
                    let return_status = realtime.bike(return_station_id);
                    let mut bike_evidence = Vec::new();
                    if let Some(status) = pickup_status {
                        let evidence = status_evidence(
                            status,
                            LiveStatusKind::BikeAvailability,
                            EvidenceType::Live,
                            bike_value(status).and_then(bike_observed_at),
                        );
                        bike_evidence.push(evidence.clone());
                        live_evidence.push(evidence);
                        required_states.push(status.freshness.state);
                    }
                    if let Some(status) = return_status {
                        let evidence = status_evidence(
                            status,
                            LiveStatusKind::BikeAvailability,
                            EvidenceType::Live,
                            bike_value(status).and_then(bike_observed_at),
                        );
                        live_evidence.push(evidence.clone());
                        bike_evidence.push(evidence);
                        required_states.push(status.freshness.state);
                    }
                    bike_availability = Some(BikeActionEvidence {
                        pickup_state: availability.pickup.rental_state,
                        return_state: availability.return_station.return_state,
                        available_bikes: availability.pickup.available_bikes,
                        available_return_slots: availability.return_station.available_return_slots,
                    });
                    let leg_id = format!("candidate_leg_{}", legs.len());
                    legs.push(bike_leg(
                        leg_id.clone(),
                        pickup,
                        return_station,
                        departure,
                        arrival,
                        &path,
                        &availability.pickup,
                        &availability.return_station,
                        bike_evidence,
                    ));
                    leg_transit_modes.push(None);
                    leg_windows.push((leg_id, departure, arrival));
                    current_time = arrival;
                    push_unique_mode(&mut travel_modes, JourneyMode::BikeShare);
                    main_service_sequence.push(format!(
                        "bike_share:{}:{}",
                        pickup_station_id.as_str(),
                        return_station_id.as_str()
                    ));
                }
                multimodal_graph::CandidateEdge::Transfer {
                    duration_seconds, ..
                } => {
                    if let Some(from_index) = leg_windows.len().checked_sub(1) {
                        pending_transfers.push((from_index, *duration_seconds));
                    }
                    current_time += Duration::seconds(i64::from(*duration_seconds));
                }
            }
        }
        if legs.is_empty() || !matches!(journey.node, multimodal_graph::CandidateNode::Destination)
        {
            return Ok(None);
        }
        let arrival_time = current_time;
        let Some(duration_seconds) = seconds_between(departure_time, arrival_time) else {
            return Ok(None);
        };
        if matches!(
            request.departure_setting,
            crate::api::DepartureSetting::ArriveBy { .. }
        ) && arrival_time
            > request
                .departure_setting
                .effective_search_time(taipei_now())
        {
            return Ok(None);
        }
        let matching_key = short_digest(&format!(
            "multimodal:v1:{}:{}",
            main_service_sequence.join(","),
            journey
                .edge_ids
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key: matching_key.clone(),
            class: JourneyClass::Reliable,
            label: None,
            ranking_reasons: Vec::new(),
            departure_time,
            arrival_time,
            duration_seconds,
            walking_seconds,
            transfer_count: journey.transfer_count,
            travel_modes,
            live_status_summary: summary_from_evidence(&live_evidence),
            overview_geometry: overview_geometry(&legs),
            legs,
            transfers,
            warnings: Vec::new(),
        };
        Ok(Some(GeneratedCandidate {
            ranking: CandidateJourney {
                stable_key: format!("{}:{}", matching_key, departure_time.unix_timestamp()),
                departure_epoch_seconds: departure_time.unix_timestamp(),
                arrival_epoch_seconds: arrival_time.unix_timestamp(),
                walking_seconds,
                transfer_count: journey.transfer_count,
                main_service_sequence,
                required_live_status: required_states,
                transfers: transfer_evidence,
                bike_availability,
                blocking_condition: false,
            },
            option,
            schedule_only,
        }))
    }

    async fn hydrate_street_edge(
        &self,
        request: &PlanningRequest,
        from: &multimodal_graph::CandidateNode,
        to: &multimodal_graph::CandidateNode,
        mode: StreetTravelMode,
        constraints: RoutingConstraints,
    ) -> Result<Option<DetailedPath>, PlanningError> {
        let Some(from) = self.node_coordinate(request, from) else {
            return Ok(None);
        };
        let Some(to) = self.node_coordinate(request, to) else {
            return Ok(None);
        };
        match self
            .street_routing
            .detailed_path(DetailedPathQuery {
                travel_mode: mode,
                origin: from,
                destination: to,
                constraints,
            })
            .await
        {
            Ok(path) => Ok(Some(path)),
            Err(ProviderError::UnreachableEndpoints) => Ok(None),
            Err(error) if is_transient_provider_error(&error) => {
                Err(PlanningError::TemporarilyUnavailable)
            }
            Err(_) => Err(PlanningError::Internal),
        }
    }

    fn node_coordinate(
        &self,
        request: &PlanningRequest,
        node: &multimodal_graph::CandidateNode,
    ) -> Option<Coordinate> {
        let coordinate = match node {
            multimodal_graph::CandidateNode::Origin => request.origin.coordinate,
            multimodal_graph::CandidateNode::Destination => request.destination.coordinate,
            multimodal_graph::CandidateNode::TransitStop(stop_id) => {
                self.transit_network.stop(stop_id)?.coordinate
            }
            multimodal_graph::CandidateNode::BikeShareStation(station_id) => {
                self.transit_network
                    .bike_share_station(station_id)?
                    .coordinate
            }
        };
        Coordinate::try_new(coordinate.latitude, coordinate.longitude).ok()
    }

    fn node_place(
        &self,
        request: &PlanningRequest,
        node: &multimodal_graph::CandidateNode,
    ) -> Option<Place> {
        match node {
            multimodal_graph::CandidateNode::Origin => Some(Place {
                display_name: request.origin.display_text.clone(),
                coordinate: request.origin.coordinate,
                kind: "selected_place",
                via_id: None,
            }),
            multimodal_graph::CandidateNode::Destination => Some(Place {
                display_name: request.destination.display_text.clone(),
                coordinate: request.destination.coordinate,
                kind: "selected_place",
                via_id: None,
            }),
            multimodal_graph::CandidateNode::TransitStop(stop_id) => {
                self.transit_network.stop(stop_id).map(transit_place)
            }
            multimodal_graph::CandidateNode::BikeShareStation(station_id) => self
                .transit_network
                .bike_share_station(station_id)
                .map(bike_place),
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

fn latest_departure_for_path(
    graph: &multimodal_graph::MultimodalGraph,
    edge_ids: &[usize],
    latest_arrival: OffsetDateTime,
) -> Option<OffsetDateTime> {
    let mut required_arrival = latest_arrival;
    for edge_id in edge_ids.iter().rev() {
        let edge = graph.edges.get(*edge_id)?;
        match edge {
            multimodal_graph::CandidateEdge::Walking { metric, .. }
            | multimodal_graph::CandidateEdge::BikeShare { metric, .. } => {
                required_arrival -= Duration::seconds(i64::from(metric.duration_seconds));
            }
            multimodal_graph::CandidateEdge::Transfer {
                duration_seconds, ..
            } => {
                required_arrival -= Duration::seconds(i64::from(*duration_seconds));
            }
            multimodal_graph::CandidateEdge::Transit { ride, .. } => {
                if ride.arrival > required_arrival {
                    return None;
                }
                required_arrival = ride.departure;
            }
        }
    }
    Some(required_arrival)
}

#[derive(Clone)]
struct GeneratedCandidate {
    ranking: CandidateJourney,
    option: JourneyOption,
    schedule_only: bool,
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

fn is_transient_provider_error(error: &ProviderError) -> bool {
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
    candidate: &candidate_discovery::TransitCandidateNetwork,
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
        .filter(|ride| transit_timing::requires_realtime_arrival(ride))
        .map(|ride| (ride.service_id.clone(), ride.board_stop_id.clone()))
        .collect::<HashSet<_>>()
        .len()
}

#[derive(Clone)]
struct EffectiveRide {
    ride: TransitRide,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
    evidence: Vec<LiveStatusEvidence>,
}

impl JourneyPlanningService {
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

    async fn generate_walking(
        &self,
        context: &PlanningContext,
    ) -> (Vec<GeneratedCandidate>, bool, bool) {
        let (Ok(origin), Ok(destination)) = (
            to_street_coordinate(context.request.origin.coordinate),
            to_street_coordinate(context.request.destination.coordinate),
        ) else {
            return (Vec::new(), false, false);
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
            return (Vec::new(), failed, global_failure);
        };
        let (departure_time, arrival_time) = context
            .request
            .departure_setting
            .resolve(context.effective_search_time, path.duration.0);
        let matching_key = walking_matching_key(&context.request);
        let option = walking_option(
            &context.request,
            path,
            departure_time,
            arrival_time,
            matching_key,
        );
        let ranking = CandidateJourney {
            stable_key: "walking".into(),
            departure_epoch_seconds: departure_time.unix_timestamp(),
            arrival_epoch_seconds: arrival_time.unix_timestamp(),
            walking_seconds: option.walking_seconds,
            transfer_count: 0,
            main_service_sequence: Vec::new(),
            required_live_status: Vec::new(),
            transfers: Vec::new(),
            bike_availability: None,
            blocking_condition: false,
        };
        (
            vec![GeneratedCandidate {
                ranking,
                option,
                schedule_only: false,
            }],
            false,
            false,
        )
    }
}

fn apply_ranked_journey(option: &mut JourneyOption, ranked: &crate::ranking::RankedJourney) {
    option.class = ranked.class;
    option.label = ranked.label;
    option.ranking_reasons = ranked.ranking_reasons.clone();
}

fn assign_result_local_ids(option: &mut JourneyOption, index: usize) {
    let suffix = short_digest(&format!("{}:{index}", option.matching_key));
    option.id = format!("option_{:02}_{}", index + 1, suffix);
    let mut ids = HashMap::new();
    for (leg_index, leg) in option.legs.iter_mut().enumerate() {
        let previous = leg.id.clone();
        leg.id = format!("leg_{:02}_{}_{suffix}", index + 1, leg_index + 1);
        ids.insert(previous, leg.id.clone());
    }
    for transfer in &mut option.transfers {
        if let Some(id) = ids.get(&transfer.from_leg_id) {
            transfer.from_leg_id = id.clone();
        }
        if let Some(id) = ids.get(&transfer.to_leg_id) {
            transfer.to_leg_id = id.clone();
        }
        transfer.connector_leg_ids = transfer
            .connector_leg_ids
            .iter()
            .filter_map(|id| ids.get(id).cloned())
            .collect();
    }
}

fn walking_leg(
    id: String,
    from: &impl PlaceLike,
    to: &impl PlaceLike,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &DetailedPath,
) -> Leg {
    Leg {
        id,
        mode: JourneyMode::Walking,
        from: from.place(),
        to: to.place(),
        departure_time,
        arrival_time,
        scheduled_departure_time: None,
        scheduled_arrival_time: None,
        duration_seconds: path.duration.0,
        distance_metres: path.distance.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: encode_polyline6(&path.geometry),
        },
        guidance: guidance(&path.maneuvers),
        details: LegDetails::Walking {
            start_fact: None,
            end_fact: None,
        },
        live_status: Vec::new(),
        warnings: Vec::new(),
        attribution: path.attribution.clone(),
    }
}

trait PlaceLike {
    fn place(&self) -> Place;
}

impl PlaceLike for crate::api::SelectedPlace {
    fn place(&self) -> Place {
        Place {
            display_name: self.display_text.clone(),
            coordinate: self.coordinate,
            kind: "selected_place",
            via_id: None,
        }
    }
}

impl PlaceLike for Place {
    fn place(&self) -> Place {
        self.clone()
    }
}

fn transit_leg(
    id: String,
    service: &crate::transit::TransitService,
    boarding: &crate::transit::TransitStop,
    alighting: &crate::transit::TransitStop,
    effective: &EffectiveRide,
) -> Leg {
    let mode = JourneyMode::Transit;
    let details = LegDetails::Transit {
        transit_mode: service.transit_mode,
        via_service_id: service.id.as_str().into(),
        via_boarding_point_id: boarding.id.as_str().into(),
        via_alighting_point_id: alighting.id.as_str().into(),
        service_label: service.label.clone(),
        headsign: service.headsign.clone(),
        stop_count: None,
    };
    let guidance_code = match service.transit_mode {
        TransitMode::Bus => "board_bus",
        TransitMode::Metro => "board_metro",
        TransitMode::Rail => "board_rail",
        TransitMode::HighSpeedRail => "board_high_speed_rail",
    };
    let (scheduled_departure_time, scheduled_arrival_time) = match service.transit_mode {
        TransitMode::Bus => (None, None),
        TransitMode::Metro | TransitMode::Rail | TransitMode::HighSpeedRail => (
            Some(effective.ride.scheduled_departure),
            Some(effective.ride.scheduled_arrival),
        ),
    };
    Leg {
        id,
        mode,
        from: transit_place(boarding),
        to: transit_place(alighting),
        departure_time: effective.departure,
        arrival_time: effective.arrival,
        scheduled_departure_time,
        scheduled_arrival_time,
        duration_seconds: seconds_between(effective.departure, effective.arrival).unwrap_or(0),
        distance_metres: approximate_distance_metres(boarding.coordinate, alighting.coordinate),
        geometry: straight_geometry(boarding.coordinate, alighting.coordinate),
        guidance: vec![Guidance {
            code: guidance_code,
            required: true,
            parameters: BTreeMap::from([
                ("service_label".into(), serde_json::json!(service.label)),
                (
                    "boarding_point_name".into(),
                    serde_json::json!(boarding.name),
                ),
            ]),
        }],
        details,
        live_status: effective.evidence.clone(),
        warnings: Vec::new(),
        attribution: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn bike_leg(
    id: String,
    pickup: &crate::transit::BikeShareStation,
    dropoff: &crate::transit::BikeShareStation,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &DetailedPath,
    pickup_availability: &crate::normalization::BikeAvailability,
    dropoff_availability: &crate::normalization::BikeAvailability,
    evidence: Vec<LiveStatusEvidence>,
) -> Leg {
    Leg {
        id,
        mode: JourneyMode::BikeShare,
        from: bike_place(pickup),
        to: bike_place(dropoff),
        departure_time,
        arrival_time,
        scheduled_departure_time: None,
        scheduled_arrival_time: None,
        duration_seconds: path.duration.0,
        distance_metres: path.distance.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: encode_polyline6(&path.geometry),
        },
        guidance: vec![Guidance {
            code: "rent_bike_share",
            required: true,
            parameters: BTreeMap::from([
                ("pickup_station_name".into(), serde_json::json!(pickup.name)),
                (
                    "return_station_name".into(),
                    serde_json::json!(dropoff.name),
                ),
            ]),
        }],
        details: LegDetails::BikeShare {
            via_pickup_station_id: pickup.id.as_str().into(),
            via_return_station_id: dropoff.id.as_str().into(),
            pickup_state: pickup_availability.rental_state,
            return_state: dropoff_availability.return_state,
            available_bikes: pickup_availability.available_bikes,
            available_return_slots: dropoff_availability.available_return_slots,
        },
        live_status: evidence,
        warnings: Vec::new(),
        attribution: path.attribution.clone(),
    }
}

fn transit_place(stop: &crate::transit::TransitStop) -> Place {
    Place {
        display_name: stop.name.clone(),
        coordinate: stop.coordinate,
        kind: "boarding_point",
        via_id: Some(stop.id.as_str().into()),
    }
}

fn bike_place(station: &crate::transit::BikeShareStation) -> Place {
    Place {
        display_name: station.name.clone(),
        coordinate: station.coordinate,
        kind: "bike_share_station",
        via_id: Some(station.id.as_str().into()),
    }
}

fn overview_geometry(legs: &[Leg]) -> Geometry {
    let mut points = Vec::new();
    for leg in legs {
        let fallback = || {
            vec![
                Coordinate::try_new(leg.from.coordinate.latitude, leg.from.coordinate.longitude)
                    .expect("validated coordinate"),
                Coordinate::try_new(leg.to.coordinate.latitude, leg.to.coordinate.longitude)
                    .expect("validated coordinate"),
            ]
        };
        for point in decode_polyline6(&leg.geometry.points).unwrap_or_else(fallback) {
            if points.last() != Some(&point) {
                points.push(point);
            }
        }
    }
    Geometry {
        encoding: "polyline6",
        points: encode_polyline6(&points),
    }
}

fn straight_geometry(from: RequestCoordinate, to: RequestCoordinate) -> Geometry {
    let from = Coordinate::try_new(from.latitude, from.longitude).expect("validated coordinate");
    let to = Coordinate::try_new(to.latitude, to.longitude).expect("validated coordinate");
    Geometry {
        encoding: "polyline6",
        points: encode_polyline6(&[from, to]),
    }
}

fn approximate_distance_metres(from: RequestCoordinate, to: RequestCoordinate) -> u32 {
    let latitude = ((from.latitude + to.latitude) / 2.0).to_radians();
    let latitude_metres = (to.latitude - from.latitude).abs() * 111_320.0;
    let longitude_metres = (to.longitude - from.longitude).abs() * 111_320.0 * latitude.cos();
    (latitude_metres.hypot(longitude_metres)).round() as u32
}

fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> Option<u32> {
    u32::try_from((to - from).whole_seconds()).ok()
}

fn push_unique_mode(modes: &mut Vec<JourneyMode>, mode: JourneyMode) {
    if !modes.contains(&mode) {
        modes.push(mode);
    }
}

fn transfer_kind(from: Option<TransitMode>, to: Option<TransitMode>) -> TransferKind {
    if from == Some(TransitMode::Metro) && to == Some(TransitMode::Metro) {
        TransferKind::MetroToMetro
    } else {
        TransferKind::BusInvolved
    }
}

fn bike_value(status: &NormalizedRealtimeValue) -> Option<&crate::normalization::BikeAvailability> {
    match &status.value {
        RealtimeValue::BikeAvailability(value) => Some(value),
        _ => None,
    }
}

fn bike_observed_at(
    availability: &crate::normalization::BikeAvailability,
) -> Option<OffsetDateTime> {
    availability
        .provider_published_at
        .or(availability.source_change_at)
        .or(Some(availability.received_at))
}

fn status_evidence(
    status: &NormalizedRealtimeValue,
    kind: LiveStatusKind,
    evidence_type: EvidenceType,
    observed_at: Option<OffsetDateTime>,
) -> LiveStatusEvidence {
    LiveStatusEvidence {
        kind,
        evidence_type,
        observed_at,
        published_at: match &status.value {
            RealtimeValue::ArrivalEstimate(arrival) => Some(arrival.transmitted_at),
            RealtimeValue::BikeAvailability(availability) => availability.provider_published_at,
            RealtimeValue::ServiceCondition(_) => status.provider_published_at,
        }
        .or(status.provider_published_at),
        received_at: status.received_at,
        evaluated_at: status.freshness.evaluated_at,
        freshness: status.freshness.clone(),
    }
}

fn summary_from_evidence(evidence: &[LiveStatusEvidence]) -> LiveStatusSummary {
    if evidence.is_empty() {
        return LiveStatusSummary {
            state: FreshnessState::NotApplicable,
            reasons: Vec::new(),
            observed_at: None,
        };
    }
    let mut reasons = evidence
        .iter()
        .filter_map(|evidence| evidence.freshness.reason)
        .collect::<Vec<_>>();
    reasons.sort_by_key(|reason| format!("{reason:?}"));
    reasons.dedup();
    let state = if evidence
        .iter()
        .any(|evidence| evidence.freshness.state == FreshnessState::Unavailable)
    {
        FreshnessState::Unavailable
    } else if evidence
        .iter()
        .any(|evidence| evidence.freshness.state == FreshnessState::Degraded)
    {
        FreshnessState::Degraded
    } else if evidence
        .iter()
        .any(|evidence| evidence.freshness.state == FreshnessState::Fresh)
    {
        FreshnessState::Fresh
    } else {
        FreshnessState::NotApplicable
    };
    let observed_at = evidence
        .iter()
        .filter_map(|evidence| evidence.observed_at)
        .min();
    LiveStatusSummary {
        state,
        reasons,
        observed_at,
    }
}

fn short_digest(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("match_{:x}", digest.finalize())[..22].to_owned()
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanningError {
    TemporarilyUnavailable,
    Internal,
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

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum PlanningOutcome {
    Options(OptionsResult),
    Empty(EmptyResult),
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JourneyOptionsResult)]
pub struct OptionsResult {
    #[serde(flatten)]
    pub metadata: ResultMetadata,
    pub journey_options: Vec<JourneyOption>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct EmptyResult {
    #[serde(flatten)]
    pub metadata: ResultMetadata,
    pub empty: SemanticObject,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ResultMetadata {
    pub api_version: &'static str,
    pub semantic_catalog_version: &'static str,
    pub request_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub effective_search_time: OffsetDateTime,
    pub time_zone: &'static str,
    pub network_revision: crate::transit::NetworkRevision,
    pub policy_versions: PolicyVersions,
    pub warnings: Vec<Warning>,
}

impl ResultMetadata {
    fn new(
        request_id: String,
        generated_at: OffsetDateTime,
        request_started_at: OffsetDateTime,
        transit_network: &TransitNetwork,
        request: &PlanningRequest,
    ) -> Self {
        let effective_search_time = request
            .departure_setting
            .effective_search_time(request_started_at);
        Self {
            api_version: crate::api::API_VERSION,
            semantic_catalog_version: crate::api::SEMANTIC_CATALOG_VERSION,
            request_id,
            generated_at,
            effective_search_time,
            time_zone: "Asia/Taipei",
            policy_versions: PolicyVersions {
                freshness: "1".into(),
                ranking: "1".into(),
                transfer_safety: "1".into(),
            },
            network_revision: transit_network.revision(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PolicyVersions {
    pub freshness: String,
    pub ranking: String,
    pub transfer_safety: String,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct JourneyOption {
    pub id: String,
    pub matching_key: String,
    pub class: JourneyClass,
    pub label: Option<JourneyLabel>,
    pub ranking_reasons: Vec<RankingReason>,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    pub duration_seconds: u32,
    pub walking_seconds: u32,
    pub transfer_count: u32,
    pub travel_modes: Vec<JourneyMode>,
    pub live_status_summary: LiveStatusSummary,
    pub overview_geometry: Geometry,
    pub legs: Vec<Leg>,
    pub transfers: Vec<Transfer>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Geometry {
    pub encoding: &'static str,
    pub points: String,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Leg {
    pub id: String,
    pub mode: JourneyMode,
    pub from: Place,
    pub to: Place,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_departure_time: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_arrival_time: Option<OffsetDateTime>,
    pub duration_seconds: u32,
    pub distance_metres: u32,
    pub geometry: Geometry,
    pub guidance: Vec<Guidance>,
    pub details: LegDetails,
    pub live_status: Vec<LiveStatusEvidence>,
    pub warnings: Vec<Warning>,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Place {
    pub display_name: String,
    pub coordinate: RequestCoordinate,
    pub kind: &'static str,
    pub via_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Guidance {
    pub code: &'static str,
    pub required: bool,
    pub parameters: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Transfer {
    pub from_leg_id: String,
    pub to_leg_id: String,
    pub connector_leg_ids: Vec<String>,
    pub duration_seconds: u32,
    pub available_seconds: u32,
    pub class: JourneyClass,
    pub reasons: Vec<RankingReason>,
    pub geometry: Option<Geometry>,
    pub guidance: Vec<Guidance>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct LiveStatusSummary {
    pub state: FreshnessState,
    pub reasons: Vec<FreshnessReason>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Warning {
    pub code: &'static str,
    pub required: bool,
    pub severity: &'static str,
    pub parameters: BTreeMap<String, serde_json::Value>,
    pub scope: WarningScope,
    pub source_text_variants: Vec<SourceTextVariant>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LegDetails {
    Walking {
        start_fact: Option<String>,
        end_fact: Option<String>,
    },
    Transit {
        transit_mode: TransitMode,
        via_service_id: String,
        via_boarding_point_id: String,
        via_alighting_point_id: String,
        service_label: String,
        headsign: String,
        stop_count: Option<u32>,
    },
    BikeShare {
        via_pickup_station_id: String,
        via_return_station_id: String,
        pickup_state: crate::normalization::ActionState,
        return_state: crate::normalization::ActionState,
        available_bikes: Option<u32>,
        available_return_slots: Option<u32>,
    },
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct LiveStatusEvidence {
    pub kind: LiveStatusKind,
    pub evidence_type: EvidenceType,
    #[serde(with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub evaluated_at: OffsetDateTime,
    pub freshness: crate::live_status::Freshness,
}

#[derive(Clone, Copy, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatusKind {
    ArrivalEstimate,
    ServiceCondition,
    BikeAvailability,
}

#[derive(Clone, Copy, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    Live,
    ScheduleOnly,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct WarningScope {
    #[serde(rename = "type")]
    pub kind: WarningScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningScopeKind {
    Result,
    JourneyOption,
    Leg,
    Transfer,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct SourceTextVariant {
    pub locale: String,
    pub text: String,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct SemanticObject {
    pub code: &'static str,
    pub required: bool,
    pub parameters: BTreeMap<&'static str, serde_json::Value>,
}

fn walking_option(
    request: &PlanningRequest,
    path: DetailedPath,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    matching_key: String,
) -> JourneyOption {
    let encoded = encode_polyline6(&path.geometry);
    let geometry = Geometry {
        encoding: "polyline6",
        points: encoded.clone(),
    };
    let leg = Leg {
        id: "candidate_leg_0".into(),
        mode: JourneyMode::Walking,
        from: Place {
            display_name: request.origin.display_text.clone(),
            coordinate: request.origin.coordinate,
            kind: "selected_place",
            via_id: None,
        },
        to: Place {
            display_name: request.destination.display_text.clone(),
            coordinate: request.destination.coordinate,
            kind: "selected_place",
            via_id: None,
        },
        departure_time,
        arrival_time,
        scheduled_departure_time: None,
        scheduled_arrival_time: None,
        duration_seconds: path.duration.0,
        distance_metres: path.distance.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: encoded,
        },
        guidance: guidance(&path.maneuvers),
        details: LegDetails::Walking {
            start_fact: None,
            end_fact: None,
        },
        live_status: Vec::new(),
        warnings: Vec::new(),
        attribution: path.attribution,
    };
    JourneyOption {
        id: "candidate".into(),
        matching_key,
        class: JourneyClass::Reliable,
        label: None,
        ranking_reasons: Vec::new(),
        departure_time,
        arrival_time,
        duration_seconds: path.duration.0,
        walking_seconds: path.duration.0,
        transfer_count: 0,
        travel_modes: vec![JourneyMode::Walking],
        live_status_summary: LiveStatusSummary {
            state: FreshnessState::NotApplicable,
            reasons: Vec::new(),
            observed_at: None,
        },
        overview_geometry: geometry,
        legs: vec![leg],
        transfers: Vec::new(),
        warnings: Vec::new(),
    }
}

fn guidance(maneuvers: &[Maneuver]) -> Vec<Guidance> {
    maneuvers
        .iter()
        .map(|maneuver| Guidance {
            code: match maneuver.kind {
                ManeuverKind::Depart => "depart",
                ManeuverKind::Continue => "continue",
                ManeuverKind::TurnLeft => "turn_left",
                ManeuverKind::TurnRight => "turn_right",
                ManeuverKind::UTurn => "u_turn",
                ManeuverKind::EnterRoundabout => "enter_roundabout",
                ManeuverKind::ExitRoundabout => "exit_roundabout",
                ManeuverKind::Arrive => "arrive",
            },
            required: true,
            parameters: BTreeMap::new(),
        })
        .collect()
}

fn walking_matching_key(request: &PlanningRequest) -> String {
    let mut digest = Sha256::new();
    digest.update(b"via.walking.v1\0");
    digest.update(request.origin.coordinate.latitude.to_be_bytes());
    digest.update(request.origin.coordinate.longitude.to_be_bytes());
    digest.update(request.destination.coordinate.latitude.to_be_bytes());
    digest.update(request.destination.coordinate.longitude.to_be_bytes());
    format!("match_{:x}", digest.finalize())
}

fn encode_polyline6(coordinates: &[Coordinate]) -> String {
    let mut output = String::new();
    let mut previous_latitude = 0_i64;
    let mut previous_longitude = 0_i64;
    for coordinate in coordinates {
        let latitude = (coordinate.latitude() * 1_000_000.0).round() as i64;
        let longitude = (coordinate.longitude() * 1_000_000.0).round() as i64;
        encode_polyline_value(latitude - previous_latitude, &mut output);
        encode_polyline_value(longitude - previous_longitude, &mut output);
        previous_latitude = latitude;
        previous_longitude = longitude;
    }
    output
}

fn encode_polyline_value(value: i64, output: &mut String) {
    let mut encoded = if value < 0 {
        (!(value << 1)) as u64
    } else {
        (value << 1) as u64
    };
    while encoded >= 0x20 {
        output.push(char::from_u32(((0x20 | (encoded & 0x1f)) + 63) as u32).unwrap());
        encoded >>= 5;
    }
    output.push(char::from_u32((encoded + 63) as u32).unwrap());
}

fn decode_polyline6(encoded: &str) -> Option<Vec<Coordinate>> {
    let mut values = Vec::new();
    let mut current = 0_i64;
    let mut shift = 0_u32;
    for byte in encoded.bytes() {
        if !(63..=126).contains(&byte) {
            return None;
        }
        let value = i64::from(byte - 63);
        current |= (value & 0x1f) << shift;
        if value < 0x20 {
            values.push(if current & 1 == 1 {
                !(current >> 1)
            } else {
                current >> 1
            });
            current = 0;
            shift = 0;
        } else {
            shift += 5;
            if shift > 30 {
                return None;
            }
        }
    }
    if shift != 0 || values.len() % 2 != 0 {
        return None;
    }
    let mut latitude = 0_i64;
    let mut longitude = 0_i64;
    let mut points = Vec::with_capacity(values.len() / 2);
    for pair in values.chunks_exact(2) {
        latitude += pair[0];
        longitude += pair[1];
        points.push(
            Coordinate::try_new(
                latitude as f64 / 1_000_000.0,
                longitude as f64 / 1_000_000.0,
            )
            .ok()?,
        );
    }
    Some(points)
}

fn to_street_coordinate(coordinate: RequestCoordinate) -> Result<Coordinate, PlanningError> {
    Coordinate::try_new(coordinate.latitude, coordinate.longitude).map_err(PlanningError::from)
}

fn taipei_now() -> OffsetDateTime {
    OffsetDateTime::now_utc().to_offset(UtcOffset::from_hms(8, 0, 0).unwrap())
}

pub fn selected_modes_as_strings(modes: &[JourneyMode]) -> Vec<&'static str> {
    modes
        .iter()
        .map(|mode| match mode {
            JourneyMode::Walking => "walking",
            JourneyMode::BikeShare => "bike_share",
            JourneyMode::Transit => "transit",
            JourneyMode::Unknown => "unknown",
        })
        .collect()
}
