use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::{
    api::{
        PlanningRequest, RequestCoordinate, SelectedMode, TravelMode, new_request_id,
        normalize_to_taipei,
    },
    live_status::{FreshnessReason, FreshnessState},
    normalization::{ArrivalState, BoardingPointId, YouBikeStationId},
    ranking::{
        BikeActionEvidence, CandidateJourney, JourneyClass, JourneyLabel, RankingPolicy,
        RankingReason, TransferEvidence, TransferKind,
    },
    snapshot::{
        LiveStatusValue, NetworkSnapshot, NormalizedLiveStatus, SnapshotError, SnapshotStore,
    },
    street::{
        Attribution, Coordinate, DetailedPath, DetailedPathQuery, Maneuver, ManeuverKind,
        MatrixOrientation, MatrixQuery, MatrixReachability, ProviderError, RoutingConstraints,
        RoutingTime, RoutingTimeKind, StreetRoutingSession, StreetTravelMode,
    },
    transit::{
        TransitAccess, TransitItinerary, TransitMode, TransitNetwork, TransitQuery, TransitRide,
    },
};

#[derive(Clone)]
pub struct JourneyPlanningService {
    street_routing: StreetRoutingSession,
    snapshots: Arc<dyn SnapshotStore>,
    transit_network: Arc<TransitNetwork>,
    ranking: RankingPolicy,
    limits: PlanningLimits,
    request_slots: Arc<tokio::sync::Semaphore>,
}

#[derive(Clone, Debug)]
pub struct PlanningLimits {
    pub request_timeout: std::time::Duration,
    pub max_concurrent_requests: usize,
    pub max_transit_access_stops: usize,
    pub max_youbike_stations: usize,
    pub max_candidates: usize,
}

impl Default for PlanningLimits {
    fn default() -> Self {
        Self {
            request_timeout: std::time::Duration::from_secs(5),
            max_concurrent_requests: 32,
            max_transit_access_stops: 24,
            max_youbike_stations: 12,
            max_candidates: 16,
        }
    }
}

impl JourneyPlanningService {
    pub fn new(street_routing: StreetRoutingSession, snapshots: Arc<dyn SnapshotStore>) -> Self {
        Self {
            street_routing,
            snapshots,
            transit_network: Arc::new(TransitNetwork::default()),
            ranking: RankingPolicy::default(),
            request_slots: Arc::new(tokio::sync::Semaphore::new(
                PlanningLimits::default().max_concurrent_requests,
            )),
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

    pub fn with_limits(mut self, limits: PlanningLimits) -> Self {
        let limits = PlanningLimits {
            max_concurrent_requests: limits.max_concurrent_requests.max(1),
            max_transit_access_stops: limits.max_transit_access_stops.max(1),
            max_youbike_stations: limits.max_youbike_stations.max(1),
            max_candidates: limits.max_candidates.max(1),
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
            self.plan_inner(request).await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(PlanningError::TemporarilyUnavailable),
        }
    }

    async fn plan_inner(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
        let request_started_at = taipei_now();
        let snapshot = self.snapshots.pin_current().await?;
        if !self.transit_network.static_network_revision.is_empty()
            && !self.transit_network.matches_snapshot(
                &snapshot.static_network_revision,
                &snapshot.schedule_revision,
            )
        {
            tracing::error!(
                snapshot_static_revision = %snapshot.static_network_revision,
                snapshot_schedule_revision = %snapshot.schedule_revision,
                artifact_static_revision = %self.transit_network.static_network_revision,
                artifact_schedule_revision = %self.transit_network.schedule_revision,
                "transit artifact revision does not match pinned Network Snapshot"
            );
            return Err(PlanningError::SnapshotUnavailable);
        }
        let metadata = ResultMetadata::new(
            new_request_id(),
            taipei_now(),
            request_started_at,
            snapshot.clone(),
            &request,
        );

        let context = PlanningContext::new(
            &request,
            snapshot.clone(),
            request_started_at,
            &self.transit_network,
        )?;
        let mut candidates = Vec::new();
        let mut had_transient_failure = false;

        let mut had_global_failure = false;
        let (walking_candidates, walking_failed, walking_global_failure) =
            self.generate_walking(&context).await;
        candidates.extend(walking_candidates);
        had_transient_failure |= walking_failed;
        had_global_failure |= walking_global_failure;

        let (transit_candidates, transit_failed, transit_global_failure) =
            self.generate_transit(&context).await;
        candidates.extend(transit_candidates);
        had_transient_failure |= transit_failed;
        had_global_failure |= transit_global_failure;

        let (bike_candidates, bike_failed, bike_global_failure) =
            self.generate_youbike(&context).await;
        candidates.extend(bike_candidates);
        had_transient_failure |= bike_failed;
        had_global_failure |= bike_global_failure;

        if had_global_failure {
            return Err(PlanningError::Internal);
        }

        if candidates.is_empty() && had_transient_failure {
            return Err(PlanningError::TemporarilyUnavailable);
        }

        let schedule_only_keys = candidates
            .iter()
            .filter(|candidate| candidate.schedule_only)
            .map(|candidate| candidate.ranking.stable_key.clone())
            .collect::<HashSet<_>>();
        let ranked = self.ranking.rank_with_schedule_only(
            candidates
                .iter()
                .map(|candidate| candidate.ranking.clone())
                .collect(),
            &schedule_only_keys,
        );
        let mut by_key = candidates
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
}

#[derive(Clone)]
struct PlanningContext {
    request: PlanningRequest,
    snapshot: NetworkSnapshot,
    effective_search_time: OffsetDateTime,
    search_start: OffsetDateTime,
    latest_arrival: Option<OffsetDateTime>,
}

impl PlanningContext {
    fn new(
        request: &PlanningRequest,
        snapshot: NetworkSnapshot,
        request_started_at: OffsetDateTime,
        transit_network: &TransitNetwork,
    ) -> Result<Self, PlanningError> {
        let effective_search_time = request
            .departure_setting
            .effective_search_time(request_started_at);
        let (search_start, latest_arrival) = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { .. } => (
                transit_network
                    .service_day_start(transit_network.service_day(effective_search_time)),
                Some(effective_search_time),
            ),
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => (effective_search_time, None),
        };
        Ok(Self {
            request: request.clone(),
            snapshot,
            effective_search_time,
            search_start,
            latest_arrival,
        })
    }

    fn local_time(&self, instant: OffsetDateTime) -> time::PrimitiveDateTime {
        let instant = normalize_to_taipei(instant);
        instant.date().with_time(instant.time())
    }

    fn constraints(&self, instant: OffsetDateTime, kind: RoutingTimeKind) -> RoutingConstraints {
        RoutingConstraints {
            time: Some(RoutingTime {
                kind,
                local_date_time: self.local_time(instant),
            }),
            max_distance: None,
        }
    }

    fn access_constraints(&self) -> RoutingConstraints {
        self.constraints(self.search_start, RoutingTimeKind::DepartAt)
    }

    fn direct_constraints(&self) -> RoutingConstraints {
        match self.latest_arrival {
            Some(latest_arrival) => self.constraints(latest_arrival, RoutingTimeKind::ArriveBy),
            None => self.constraints(self.search_start, RoutingTimeKind::DepartAt),
        }
    }
}

#[derive(Clone)]
struct GeneratedCandidate {
    ranking: CandidateJourney,
    option: JourneyOption,
    schedule_only: bool,
}

#[derive(Clone)]
struct AccessPath {
    path: DetailedPath,
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

#[derive(Clone, Copy)]
struct MatrixMetric {
    duration: crate::street::Seconds,
    distance: crate::street::Metres,
}

fn matrix_metric(cell: &crate::street::MatrixCell) -> Option<MatrixMetric> {
    match cell.reachability {
        MatrixReachability::Reachable { duration, distance } => {
            Some(MatrixMetric { duration, distance })
        }
        MatrixReachability::Unreachable => None,
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

#[derive(Clone)]
struct EffectiveRide {
    ride: TransitRide,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
    evidence: Vec<LiveStatusEvidence>,
    required_states: Vec<FreshnessState>,
    schedule_only: bool,
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

    async fn generate_transit(
        &self,
        context: &PlanningContext,
    ) -> (Vec<GeneratedCandidate>, bool, bool) {
        if !context
            .request
            .selected_modes
            .iter()
            .any(|mode| matches!(mode, SelectedMode::Bus | SelectedMode::Metro))
        {
            return (Vec::new(), false, false);
        }
        let Ok(origin) = to_street_coordinate(context.request.origin.coordinate) else {
            return (Vec::new(), false, false);
        };
        let Ok(destination) = to_street_coordinate(context.request.destination.coordinate) else {
            return (Vec::new(), false, false);
        };
        let mut had_transient_failure = false;
        let mut had_global_failure = false;
        let stops = self
            .transit_network
            .nearest_stops(
                context.request.origin.coordinate,
                self.limits.max_transit_access_stops,
            )
            .into_iter()
            .filter_map(|stop| {
                to_street_coordinate(stop.coordinate)
                    .ok()
                    .map(|coordinate| (stop.clone(), coordinate))
            })
            .collect::<Vec<_>>();
        if stops.is_empty() {
            return (Vec::new(), false, false);
        }
        let access_matrix = self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![origin],
                targets: stops.iter().map(|(_, coordinate)| *coordinate).collect(),
                constraints: context.access_constraints(),
            })
            .await;
        let access_matrix = match access_matrix {
            Ok(matrix) => matrix,
            Err(error) => {
                had_transient_failure |= is_transient_provider_error(&error);
                had_global_failure |= is_global_provider_error(&error);
                return (Vec::new(), had_transient_failure, had_global_failure);
            }
        };
        let access = access_matrix
            .cells
            .first()
            .into_iter()
            .flatten()
            .filter_map(|cell| {
                let (stop, _) = stops.get(cell.target_index)?;
                let metric = matrix_metric(cell)?;
                let available_at =
                    context.search_start + Duration::seconds(i64::from(metric.duration.0));
                Some(TransitAccess::new(
                    stop.id.clone(),
                    context.search_start,
                    available_at,
                ))
            })
            .collect::<Vec<_>>();

        let query = match context.latest_arrival {
            Some(latest_arrival) => TransitQuery::ArriveBy { latest_arrival },
            None => TransitQuery::DepartAfter {
                earliest_departure: context.effective_search_time,
            },
        };
        let itineraries =
            self.transit_network
                .itineraries(&access, query, &context.request.selected_modes);
        let mut egress_stops = Vec::new();
        let mut egress_seen = HashSet::new();
        for itinerary in itineraries.iter().take(self.limits.max_candidates) {
            let Some(ride) = itinerary.rides.last() else {
                continue;
            };
            if egress_seen.insert(ride.alight_stop_id.clone()) {
                if let Some(stop) = self.transit_network.stop(&ride.alight_stop_id) {
                    if let Ok(coordinate) = to_street_coordinate(stop.coordinate) {
                        egress_stops.push((ride.alight_stop_id.clone(), coordinate));
                    }
                }
            }
        }
        if egress_stops.is_empty() {
            return (Vec::new(), had_transient_failure, had_global_failure);
        }
        let egress_matrix = self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToOne,
                travel_mode: StreetTravelMode::Walking,
                sources: egress_stops
                    .iter()
                    .map(|(_, coordinate)| *coordinate)
                    .collect(),
                targets: vec![destination],
                constraints: context.direct_constraints(),
            })
            .await;
        let egress_matrix = match egress_matrix {
            Ok(matrix) => matrix,
            Err(error) => {
                had_transient_failure |= is_transient_provider_error(&error);
                had_global_failure |= is_global_provider_error(&error);
                return (Vec::new(), had_transient_failure, had_global_failure);
            }
        };
        let mut egress_metrics = HashMap::new();
        for (source_index, cells) in egress_matrix.cells.iter().enumerate() {
            if let Some(metric) = cells.first().and_then(matrix_metric) {
                if let Some((stop_id, _)) = egress_stops.get(source_index) {
                    egress_metrics.insert(stop_id.clone(), metric);
                }
            }
        }
        let mut candidates = Vec::new();
        for itinerary in itineraries.into_iter().take(self.limits.max_candidates) {
            let Some(first_ride) = itinerary.rides.first() else {
                continue;
            };
            let Some(first_stop) = self.transit_network.stop(&first_ride.board_stop_id) else {
                continue;
            };
            let Some(last_ride) = itinerary.rides.last() else {
                continue;
            };
            let Some(_egress_metric) = egress_metrics.get(&last_ride.alight_stop_id) else {
                continue;
            };
            let Ok(first_coordinate) = to_street_coordinate(first_stop.coordinate) else {
                continue;
            };
            let access_attempt = self
                .street_path(
                    StreetTravelMode::Walking,
                    origin,
                    first_coordinate,
                    context.access_constraints(),
                )
                .await;
            had_transient_failure |= access_attempt.was_transient_failure();
            had_global_failure |= access_attempt.is_global_failure();
            let PathAttempt::Found(access_path) = access_attempt else {
                continue;
            };
            let Some(last_stop) = self.transit_network.stop(&last_ride.alight_stop_id) else {
                continue;
            };
            let Ok(last_coordinate) = to_street_coordinate(last_stop.coordinate) else {
                continue;
            };
            let egress_attempt = self
                .street_path(
                    StreetTravelMode::Walking,
                    last_coordinate,
                    destination,
                    context.constraints(itinerary.arrival_at, RoutingTimeKind::DepartAt),
                )
                .await;
            had_transient_failure |= egress_attempt.was_transient_failure();
            had_global_failure |= egress_attempt.is_global_failure();
            let PathAttempt::Found(egress_path) = egress_attempt else {
                continue;
            };
            if let Some(candidate) = self.assemble_transit_candidate(
                context,
                itinerary,
                &AccessPath { path: access_path },
                egress_path,
            ) {
                candidates.push(candidate);
            }
        }
        (candidates, had_transient_failure, had_global_failure)
    }

    fn assemble_transit_candidate(
        &self,
        context: &PlanningContext,
        itinerary: TransitItinerary,
        access_path: &AccessPath,
        egress_path: DetailedPath,
    ) -> Option<GeneratedCandidate> {
        let mut effective_rides = Vec::with_capacity(itinerary.rides.len());
        let trip_signature = itinerary
            .rides
            .iter()
            .map(|ride| ride.trip_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        for ride in itinerary.rides {
            let service = self.transit_network.service(&ride.service_id)?;
            let condition = find_service_condition(&context.snapshot, service.id.as_str());
            let mut effective = EffectiveRide {
                ride: ride.clone(),
                departure: ride.scheduled_departure,
                arrival: ride.scheduled_arrival,
                evidence: Vec::new(),
                required_states: Vec::new(),
                schedule_only: false,
            };
            if let Some(status) = condition {
                if status.freshness.state != FreshnessState::Fresh
                    || service_condition_blocks(status, context.snapshot.live_cutoff)
                {
                    return None;
                }
                effective.required_states.push(status.freshness.state);
                effective.evidence.push(status_evidence(
                    status,
                    LiveStatusKind::ServiceCondition,
                    EvidenceType::Live,
                    None,
                ));
            }
            match ride.mode {
                TransitMode::Bus => {
                    let status = find_arrival_status(&context.snapshot, &ride)?;
                    if status.freshness.state == FreshnessState::Unavailable {
                        return None;
                    }
                    effective.required_states.push(status.freshness.state);
                    let LiveStatusValue::ArrivalEstimate(arrival) = &status.value else {
                        return None;
                    };
                    let predicted = arrival.predicted_arrival_at?;
                    if !matches!(
                        arrival.state,
                        ArrivalState::Predicted | ArrivalState::AtBoardingPoint
                    ) {
                        return None;
                    }
                    let delay = predicted - ride.scheduled_departure;
                    effective.departure = ride.scheduled_departure + delay;
                    effective.arrival = ride.scheduled_arrival + delay;
                    if effective.arrival < effective.departure {
                        return None;
                    }
                    effective.evidence.push(status_evidence(
                        status,
                        LiveStatusKind::ArrivalEstimate,
                        EvidenceType::Live,
                        Some(arrival.transmitted_at),
                    ));
                }
                TransitMode::Metro => {
                    if service.service_condition_required && condition.is_none() {
                        return None;
                    }
                    if condition.is_none() {
                        effective.schedule_only = true;
                        effective.evidence.push(schedule_only_evidence(
                            &context.snapshot,
                            LiveStatusKind::ServiceCondition,
                        ));
                    }
                }
            }
            effective_rides.push(effective);
        }

        let first = effective_rides.first()?;
        let access_departure = if context.latest_arrival.is_some() {
            first.departure - Duration::seconds(i64::from(access_path.path.duration.0))
        } else {
            context.search_start
        };
        let access_departure = access_departure.max(context.search_start);
        let access_arrival = access_path.path_duration_arrival(access_departure);
        if first.departure < access_arrival {
            return None;
        }
        let last = effective_rides.last()?;
        let egress_departure = last.arrival;
        let arrival_time = egress_departure + Duration::seconds(i64::from(egress_path.duration.0));
        if context
            .latest_arrival
            .is_some_and(|latest| arrival_time > latest)
        {
            return None;
        }

        let mut legs = vec![walking_leg(
            "candidate_leg_0".into(),
            &context.request.origin,
            &stop_place_for_ride(&self.transit_network, &first.ride.board_stop_id)?,
            access_departure,
            access_arrival,
            &access_path.path,
        )];
        let mut transfer_evidence = Vec::new();
        let mut transfers = Vec::new();
        let mut live_evidence = Vec::new();
        let mut required_states = Vec::new();
        let mut schedule_only = false;
        let mut travel_modes = vec![TravelMode::Walking];
        let mut main_service_sequence = Vec::new();

        for (index, effective) in effective_rides.iter().enumerate() {
            let service = self.transit_network.service(&effective.ride.service_id)?;
            let boarding = self.transit_network.stop(&effective.ride.board_stop_id)?;
            let alighting = self.transit_network.stop(&effective.ride.alight_stop_id)?;
            let leg_id = format!("candidate_leg_{}", index + 1);
            legs.push(transit_leg(
                leg_id.clone(),
                service,
                boarding,
                alighting,
                effective,
            ));
            push_unique_mode(&mut travel_modes, travel_mode_for(service.mode));
            main_service_sequence.push(service.id.as_str().to_owned());
            required_states.extend(effective.required_states.iter().copied());
            live_evidence.extend(effective.evidence.iter().cloned());
            schedule_only |= effective.schedule_only;

            if let Some(previous) = index
                .checked_sub(1)
                .and_then(|previous_index| effective_rides.get(previous_index))
            {
                let available = seconds_between(previous.arrival, effective.departure)?;
                let physical = self.transit_network.transfer_seconds(
                    &previous.ride.alight_stop_id,
                    &effective.ride.board_stop_id,
                )?;
                if available < physical {
                    return None;
                }
                let kind = if matches!(previous.ride.mode, TransitMode::Metro)
                    && matches!(effective.ride.mode, TransitMode::Metro)
                {
                    TransferKind::MetroToMetro
                } else {
                    TransferKind::BusInvolved
                };
                transfer_evidence.push(TransferEvidence {
                    kind,
                    physical_seconds: physical,
                    available_seconds: available,
                });
                transfers.push(Transfer {
                    from_leg_id: format!("candidate_leg_{}", index),
                    to_leg_id: leg_id,
                    connector_leg_ids: Vec::new(),
                    duration_seconds: physical,
                    available_seconds: available,
                    class: if transfer_is_reliable(kind, physical, available) {
                        JourneyClass::Reliable
                    } else {
                        JourneyClass::Feasible
                    },
                    reasons: if transfer_is_reliable(kind, physical, available) {
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
        let destination_place = Place {
            display_name: context.request.destination.display_text.clone(),
            coordinate: context.request.destination.coordinate,
            kind: "selected_place",
            via_id: None,
        };
        legs.push(walking_leg(
            format!("candidate_leg_{}", legs.len()),
            &stop_place_for_ride(&self.transit_network, &last.ride.alight_stop_id)?,
            &destination_place,
            egress_departure,
            arrival_time,
            &egress_path,
        ));
        push_unique_mode(&mut travel_modes, TravelMode::Walking);

        let departure_time = access_departure;
        let duration_seconds = seconds_between(departure_time, arrival_time)?;
        let walking_seconds = access_path.path.duration.0 + egress_path.duration.0;
        let matching_key = transit_matching_key(
            &main_service_sequence,
            &first.ride.board_stop_id,
            &last.ride.alight_stop_id,
        );
        let stable_key = format!(
            "{}:{}:{}",
            matching_key,
            first.ride.scheduled_departure.unix_timestamp(),
            trip_signature
        );
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key,
            class: JourneyClass::Reliable,
            label: None,
            ranking_reasons: Vec::new(),
            departure_time,
            arrival_time,
            duration_seconds,
            walking_seconds,
            transfer_count: transfer_evidence.len() as u32,
            travel_modes,
            live_status_summary: summary_from_evidence(&live_evidence),
            overview_geometry: overview_geometry(&legs),
            legs,
            transfers,
            warnings: Vec::new(),
        };
        Some(GeneratedCandidate {
            ranking: CandidateJourney {
                stable_key,
                departure_epoch_seconds: departure_time.unix_timestamp(),
                arrival_epoch_seconds: arrival_time.unix_timestamp(),
                walking_seconds,
                transfer_count: transfer_evidence.len() as u32,
                main_service_sequence,
                required_live_status: required_states,
                transfers: transfer_evidence,
                bike_availability: None,
                blocking_condition: false,
            },
            option,
            schedule_only,
        })
    }

    async fn generate_youbike(
        &self,
        context: &PlanningContext,
    ) -> (Vec<GeneratedCandidate>, bool, bool) {
        if !context
            .request
            .selected_modes
            .contains(&SelectedMode::YouBike)
        {
            return (Vec::new(), false, false);
        }
        let Ok(origin) = to_street_coordinate(context.request.origin.coordinate) else {
            return (Vec::new(), false, false);
        };
        let Ok(destination) = to_street_coordinate(context.request.destination.coordinate) else {
            return (Vec::new(), false, false);
        };
        let pickups = self
            .transit_network
            .nearest_youbike_stations(
                context.request.origin.coordinate,
                self.limits.max_youbike_stations,
            )
            .into_iter()
            .filter_map(|station| {
                to_street_coordinate(station.coordinate)
                    .ok()
                    .map(|coordinate| (station.clone(), coordinate))
            })
            .collect::<Vec<_>>();
        let dropoffs = self
            .transit_network
            .nearest_youbike_stations(
                context.request.destination.coordinate,
                self.limits.max_youbike_stations,
            )
            .into_iter()
            .filter_map(|station| {
                to_street_coordinate(station.coordinate)
                    .ok()
                    .map(|coordinate| (station.clone(), coordinate))
            })
            .collect::<Vec<_>>();
        if pickups.is_empty() || dropoffs.is_empty() {
            return (Vec::new(), false, false);
        }
        let mut had_transient_failure = false;
        let mut had_global_failure = false;
        let access_matrix = match self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![origin],
                targets: pickups.iter().map(|(_, coordinate)| *coordinate).collect(),
                constraints: context.access_constraints(),
            })
            .await
        {
            Ok(matrix) => matrix,
            Err(error) => {
                had_transient_failure |= is_transient_provider_error(&error);
                had_global_failure |= is_global_provider_error(&error);
                return (Vec::new(), had_transient_failure, had_global_failure);
            }
        };
        let mut access_metrics = HashMap::new();
        for (index, cell) in access_matrix
            .cells
            .first()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if let (Some((station, _)), Some(metric)) = (pickups.get(index), matrix_metric(cell)) {
                access_metrics.insert(station.id.clone(), metric);
            }
        }
        let egress_matrix = match self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToOne,
                travel_mode: StreetTravelMode::Walking,
                sources: dropoffs.iter().map(|(_, coordinate)| *coordinate).collect(),
                targets: vec![destination],
                constraints: context.direct_constraints(),
            })
            .await
        {
            Ok(matrix) => matrix,
            Err(error) => {
                had_transient_failure |= is_transient_provider_error(&error);
                had_global_failure |= is_global_provider_error(&error);
                return (Vec::new(), had_transient_failure, had_global_failure);
            }
        };
        let mut egress_metrics = HashMap::new();
        for (index, cells) in egress_matrix.cells.iter().enumerate() {
            if let (Some((station, _)), Some(metric)) =
                (dropoffs.get(index), cells.first().and_then(matrix_metric))
            {
                egress_metrics.insert(station.id.clone(), metric);
            }
        }

        let mut metric_candidates = Vec::new();
        for (pickup_index, (pickup, pickup_coordinate)) in pickups.iter().enumerate() {
            let Some(access) = access_metrics.get(&pickup.id).copied() else {
                continue;
            };
            let bike_matrix = match self
                .street_routing
                .matrix(MatrixQuery {
                    orientation: MatrixOrientation::OneToMany,
                    travel_mode: StreetTravelMode::YouBike,
                    sources: vec![*pickup_coordinate],
                    targets: dropoffs.iter().map(|(_, coordinate)| *coordinate).collect(),
                    constraints: context.constraints(
                        context.search_start + Duration::seconds(i64::from(access.duration.0)),
                        RoutingTimeKind::DepartAt,
                    ),
                })
                .await
            {
                Ok(matrix) => matrix,
                Err(error) => {
                    had_transient_failure |= is_transient_provider_error(&error);
                    had_global_failure |= is_global_provider_error(&error);
                    continue;
                }
            };
            for (dropoff_index, cell) in bike_matrix.cells.first().into_iter().flatten().enumerate()
            {
                let Some(dropoff) = dropoffs.get(dropoff_index) else {
                    continue;
                };
                if pickup.id == dropoff.0.id || !bike_metric_is_sensible(cell) {
                    continue;
                }
                let Some(bike) = matrix_metric(cell) else {
                    continue;
                };
                let Some(egress) = egress_metrics.get(&dropoff.0.id).copied() else {
                    continue;
                };
                if !usable_bike_station(&context.snapshot, pickup)
                    || !usable_return_station(&context.snapshot, &dropoff.0)
                {
                    continue;
                }
                metric_candidates.push(BikeMetricCandidate {
                    pickup_index,
                    dropoff_index,
                    total_seconds: access.duration.0 + bike.duration.0 + egress.duration.0,
                });
            }
        }
        metric_candidates.sort_by(|left, right| {
            left.total_seconds
                .cmp(&right.total_seconds)
                .then(left.pickup_index.cmp(&right.pickup_index))
                .then(left.dropoff_index.cmp(&right.dropoff_index))
        });
        metric_candidates.truncate(self.limits.max_candidates);
        let mut candidates = Vec::new();
        for pair in metric_candidates {
            let (pickup, pickup_coordinate) = &pickups[pair.pickup_index];
            let (dropoff, dropoff_coordinate) = &dropoffs[pair.dropoff_index];
            let access_attempt = self
                .street_path(
                    StreetTravelMode::Walking,
                    origin,
                    *pickup_coordinate,
                    context.access_constraints(),
                )
                .await;
            had_transient_failure |= access_attempt.was_transient_failure();
            had_global_failure |= access_attempt.is_global_failure();
            let PathAttempt::Found(access_path) = access_attempt else {
                continue;
            };
            let bike_attempt = self
                .street_path(
                    StreetTravelMode::YouBike,
                    *pickup_coordinate,
                    *dropoff_coordinate,
                    context.constraints(
                        context.search_start + Duration::seconds(i64::from(access_path.duration.0)),
                        RoutingTimeKind::DepartAt,
                    ),
                )
                .await;
            had_transient_failure |= bike_attempt.was_transient_failure();
            had_global_failure |= bike_attempt.is_global_failure();
            let PathAttempt::Found(bike_path) = bike_attempt else {
                continue;
            };
            let egress_attempt = self
                .street_path(
                    StreetTravelMode::Walking,
                    *dropoff_coordinate,
                    destination,
                    context.constraints(
                        context.search_start
                            + Duration::seconds(i64::from(
                                access_path.duration.0 + bike_path.duration.0,
                            )),
                        RoutingTimeKind::DepartAt,
                    ),
                )
                .await;
            had_transient_failure |= egress_attempt.was_transient_failure();
            had_global_failure |= egress_attempt.is_global_failure();
            let PathAttempt::Found(egress_path) = egress_attempt else {
                continue;
            };
            if let Some(candidate) = assemble_youbike_candidate(
                context,
                pickup,
                dropoff,
                &access_path,
                bike_path,
                egress_path,
            ) {
                candidates.push(candidate);
            }
        }
        (candidates, had_transient_failure, had_global_failure)
    }
}

impl AccessPath {
    fn path_duration_arrival(&self, departure: OffsetDateTime) -> OffsetDateTime {
        departure + Duration::seconds(i64::from(self.path.duration.0))
    }
}

#[derive(Clone, Copy)]
struct BikeMetricCandidate {
    pickup_index: usize,
    dropoff_index: usize,
    total_seconds: u32,
}

fn bike_metric_is_sensible(cell: &crate::street::MatrixCell) -> bool {
    matrix_metric(cell)
        .is_some_and(|metric| metric.distance.0 <= 20_000 && metric.duration.0 <= 2 * 60 * 60)
}

fn usable_bike_station(
    snapshot: &NetworkSnapshot,
    station: &crate::transit::YouBikeStation,
) -> bool {
    find_bike_status(snapshot, &station.id).is_some_and(|status| {
        status.freshness.state == FreshnessState::Fresh
            && bike_value(status).is_some_and(|availability| {
                availability.rental_state == crate::normalization::ActionState::Enabled
                    && availability.available_bikes.is_some_and(|count| count > 0)
            })
    })
}

fn usable_return_station(
    snapshot: &NetworkSnapshot,
    station: &crate::transit::YouBikeStation,
) -> bool {
    find_bike_status(snapshot, &station.id).is_some_and(|status| {
        status.freshness.state == FreshnessState::Fresh
            && bike_value(status).is_some_and(|availability| {
                availability.return_state == crate::normalization::ActionState::Enabled
                    && availability
                        .available_return_slots
                        .is_some_and(|count| count > 0)
            })
    })
}

fn assemble_youbike_candidate(
    context: &PlanningContext,
    pickup: &crate::transit::YouBikeStation,
    dropoff: &crate::transit::YouBikeStation,
    access_path: &DetailedPath,
    bike_path: DetailedPath,
    egress_path: DetailedPath,
) -> Option<GeneratedCandidate> {
    let pickup_status = find_bike_status(&context.snapshot, &pickup.id)?;
    let dropoff_status = find_bike_status(&context.snapshot, &dropoff.id)?;
    let pickup_availability = bike_value(pickup_status)?;
    let dropoff_availability = bike_value(dropoff_status)?;
    let total_travel_seconds = i64::from(access_path.duration.0)
        + i64::from(bike_path.duration.0)
        + i64::from(egress_path.duration.0);
    let departure_time = context
        .latest_arrival
        .map(|latest| latest - Duration::seconds(total_travel_seconds))
        .unwrap_or(context.search_start);
    if departure_time < context.search_start {
        return None;
    }
    let access_arrival = departure_time + Duration::seconds(i64::from(access_path.duration.0));
    let bike_departure = access_arrival;
    let bike_arrival = bike_departure + Duration::seconds(i64::from(bike_path.duration.0));
    let arrival_time = bike_arrival + Duration::seconds(i64::from(egress_path.duration.0));
    if context
        .latest_arrival
        .is_some_and(|latest| arrival_time > latest)
    {
        return None;
    }
    let pickup_evidence = status_evidence(
        pickup_status,
        LiveStatusKind::BikeAvailability,
        EvidenceType::Live,
        bike_observed_at(pickup_availability),
    );
    let dropoff_evidence = status_evidence(
        dropoff_status,
        LiveStatusKind::BikeAvailability,
        EvidenceType::Live,
        bike_observed_at(dropoff_availability),
    );
    let pickup_place = bike_place(pickup);
    let dropoff_place = bike_place(dropoff);
    let legs = vec![
        walking_leg(
            "candidate_leg_0".into(),
            &context.request.origin,
            &pickup_place,
            departure_time,
            access_arrival,
            access_path,
        ),
        bike_leg(
            "candidate_leg_1".into(),
            pickup,
            dropoff,
            bike_departure,
            bike_arrival,
            &bike_path,
            pickup_availability,
            dropoff_availability,
            vec![pickup_evidence.clone(), dropoff_evidence.clone()],
        ),
        walking_leg(
            "candidate_leg_2".into(),
            &dropoff_place,
            &Place {
                display_name: context.request.destination.display_text.clone(),
                coordinate: context.request.destination.coordinate,
                kind: "selected_place",
                via_id: None,
            },
            bike_arrival,
            arrival_time,
            &egress_path,
        ),
    ];
    let matching_key = youbike_matching_key(&pickup.id, &dropoff.id);
    let stable_key = format!("{}:{}", matching_key, departure_time.unix_timestamp());
    let walking_seconds = access_path.duration.0 + egress_path.duration.0;
    let duration_seconds = seconds_between(departure_time, arrival_time)?;
    let option = JourneyOption {
        id: "candidate".into(),
        matching_key,
        class: JourneyClass::Reliable,
        label: None,
        ranking_reasons: Vec::new(),
        departure_time,
        arrival_time,
        duration_seconds,
        walking_seconds,
        transfer_count: 0,
        travel_modes: vec![TravelMode::Walking, TravelMode::YouBike],
        live_status_summary: summary_from_evidence(&[pickup_evidence, dropoff_evidence]),
        overview_geometry: overview_geometry(&legs),
        legs,
        transfers: Vec::new(),
        warnings: Vec::new(),
    };
    Some(GeneratedCandidate {
        ranking: CandidateJourney {
            stable_key,
            departure_epoch_seconds: departure_time.unix_timestamp(),
            arrival_epoch_seconds: arrival_time.unix_timestamp(),
            walking_seconds,
            transfer_count: 0,
            main_service_sequence: vec![format!("youbike:{}", pickup.id.as_str())],
            required_live_status: vec![
                pickup_status.freshness.state,
                dropoff_status.freshness.state,
            ],
            transfers: Vec::new(),
            bike_availability: Some(BikeActionEvidence {
                pickup_state: pickup_availability.rental_state,
                return_state: dropoff_availability.return_state,
                available_bikes: pickup_availability.available_bikes,
                available_return_slots: dropoff_availability.available_return_slots,
            }),
            blocking_condition: false,
        },
        option,
        schedule_only: false,
    })
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
        mode: TravelMode::Walking,
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
    let mode = travel_mode_for(service.mode);
    let details = match service.mode {
        TransitMode::Bus => LegDetails::Bus {
            via_service_id: service.id.as_str().into(),
            via_boarding_point_id: boarding.id.as_str().into(),
            via_alighting_point_id: alighting.id.as_str().into(),
            service_label: service.label.clone(),
            headsign: service.headsign.clone(),
            stop_count: None,
        },
        TransitMode::Metro => LegDetails::Metro {
            via_line_id: service.id.as_str().into(),
            via_boarding_point_id: boarding.id.as_str().into(),
            via_alighting_point_id: alighting.id.as_str().into(),
            line_label: service.label.clone(),
            headsign: service.headsign.clone(),
        },
    };
    let guidance_code = if matches!(service.mode, TransitMode::Bus) {
        "board_bus"
    } else {
        "board_metro"
    };
    Leg {
        id,
        mode,
        from: transit_place(boarding),
        to: transit_place(alighting),
        departure_time: effective.departure,
        arrival_time: effective.arrival,
        scheduled_departure_time: Some(effective.ride.scheduled_departure),
        scheduled_arrival_time: Some(effective.ride.scheduled_arrival),
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

fn travel_mode_for(mode: TransitMode) -> TravelMode {
    match mode {
        TransitMode::Bus => TravelMode::Bus,
        TransitMode::Metro => TravelMode::Metro,
    }
}

#[allow(clippy::too_many_arguments)]
fn bike_leg(
    id: String,
    pickup: &crate::transit::YouBikeStation,
    dropoff: &crate::transit::YouBikeStation,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &DetailedPath,
    pickup_availability: &crate::normalization::BikeAvailability,
    dropoff_availability: &crate::normalization::BikeAvailability,
    evidence: Vec<LiveStatusEvidence>,
) -> Leg {
    Leg {
        id,
        mode: TravelMode::YouBike,
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
            code: "rent_youbike",
            required: true,
            parameters: BTreeMap::from([
                ("pickup_station_name".into(), serde_json::json!(pickup.name)),
                (
                    "return_station_name".into(),
                    serde_json::json!(dropoff.name),
                ),
            ]),
        }],
        details: LegDetails::YouBike {
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

fn stop_place_for_ride(network: &TransitNetwork, id: &BoardingPointId) -> Option<Place> {
    network.stop(id).map(transit_place)
}

fn bike_place(station: &crate::transit::YouBikeStation) -> Place {
    Place {
        display_name: station.name.clone(),
        coordinate: station.coordinate,
        kind: "youbike_station",
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

fn push_unique_mode(modes: &mut Vec<TravelMode>, mode: TravelMode) {
    if !modes.contains(&mode) {
        modes.push(mode);
    }
}

fn transfer_is_reliable(kind: TransferKind, physical: u32, available: u32) -> bool {
    let buffer = match kind {
        TransferKind::MetroToMetro => 180,
        TransferKind::BusInvolved => 300,
    };
    available >= physical.saturating_add(buffer)
}

fn find_arrival_status<'a>(
    snapshot: &'a NetworkSnapshot,
    ride: &TransitRide,
) -> Option<&'a NormalizedLiveStatus> {
    snapshot.live_status.iter().find(|status| {
        matches!(
            &status.value,
            LiveStatusValue::ArrivalEstimate(arrival)
                if arrival.transit_service_id == ride.service_id
                    && arrival.boarding_point_id == ride.board_stop_id
        )
    })
}

fn find_service_condition<'a>(
    snapshot: &'a NetworkSnapshot,
    service_id: &str,
) -> Option<&'a NormalizedLiveStatus> {
    snapshot.live_status.iter().find(|status| {
        matches!(
            &status.value,
            LiveStatusValue::ServiceCondition(condition)
                if condition.scope_via_ids.iter().any(|id| id == service_id)
        )
    })
}

fn service_condition_blocks(status: &NormalizedLiveStatus, at: OffsetDateTime) -> bool {
    let LiveStatusValue::ServiceCondition(condition) = &status.value else {
        return false;
    };
    condition
        .effective_from
        .is_none_or(|effective_from| effective_from <= at)
        && condition
            .effective_until
            .is_none_or(|effective_until| at < effective_until)
}

fn find_bike_status<'a>(
    snapshot: &'a NetworkSnapshot,
    station_id: &YouBikeStationId,
) -> Option<&'a NormalizedLiveStatus> {
    snapshot.live_status.iter().find(|status| {
        matches!(
            &status.value,
            LiveStatusValue::BikeAvailability(availability)
                if &availability.station_id == station_id
        )
    })
}

fn bike_value(status: &NormalizedLiveStatus) -> Option<&crate::normalization::BikeAvailability> {
    match &status.value {
        LiveStatusValue::BikeAvailability(value) => Some(value),
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
    status: &NormalizedLiveStatus,
    kind: LiveStatusKind,
    evidence_type: EvidenceType,
    observed_at: Option<OffsetDateTime>,
) -> LiveStatusEvidence {
    LiveStatusEvidence {
        kind,
        evidence_type,
        observed_at,
        published_at: match &status.value {
            LiveStatusValue::ArrivalEstimate(arrival) => Some(arrival.transmitted_at),
            LiveStatusValue::BikeAvailability(availability) => availability.provider_published_at,
            LiveStatusValue::ServiceCondition(_) => None,
        },
        received_at: status.received_at,
        evaluated_at: status.freshness.evaluated_at,
        freshness: status.freshness.clone(),
    }
}

fn schedule_only_evidence(snapshot: &NetworkSnapshot, kind: LiveStatusKind) -> LiveStatusEvidence {
    LiveStatusEvidence {
        kind,
        evidence_type: EvidenceType::ScheduleOnly,
        observed_at: None,
        published_at: None,
        received_at: snapshot.live_cutoff,
        evaluated_at: snapshot.live_cutoff,
        freshness: crate::live_status::Freshness {
            state: FreshnessState::NotApplicable,
            reason: Some(FreshnessReason::ScheduleOnly),
            policy_version: snapshot.freshness_policy_version.clone(),
            evaluated_at: snapshot.live_cutoff,
        },
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

fn transit_matching_key(
    services: &[String],
    boarding: &BoardingPointId,
    alighting: &BoardingPointId,
) -> String {
    short_digest(&format!(
        "transit:v1:{}:{}:{}",
        services.join(","),
        boarding.as_str(),
        alighting.as_str()
    ))
}

fn youbike_matching_key(pickup: &YouBikeStationId, dropoff: &YouBikeStationId) -> String {
    short_digest(&format!(
        "youbike:v1:{}:{}",
        pickup.as_str(),
        dropoff.as_str()
    ))
}

fn short_digest(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("match_{:x}", digest.finalize())[..22].to_owned()
}

#[derive(Debug)]
pub enum PlanningError {
    SnapshotUnavailable,
    TemporarilyUnavailable,
    Internal,
}

impl From<SnapshotError> for PlanningError {
    fn from(_: SnapshotError) -> Self {
        Self::SnapshotUnavailable
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
    pub network_snapshot: NetworkSnapshot,
    pub policy_versions: PolicyVersions,
    pub warnings: Vec<Warning>,
}

impl ResultMetadata {
    fn new(
        request_id: String,
        generated_at: OffsetDateTime,
        request_started_at: OffsetDateTime,
        network_snapshot: NetworkSnapshot,
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
                freshness: network_snapshot.freshness_policy_version.clone(),
                ranking: "1".into(),
                transfer_safety: "1".into(),
            },
            network_snapshot,
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
    pub travel_modes: Vec<TravelMode>,
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
    pub mode: TravelMode,
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
    Bus {
        via_service_id: String,
        via_boarding_point_id: String,
        via_alighting_point_id: String,
        service_label: String,
        headsign: String,
        stop_count: Option<u32>,
    },
    Metro {
        via_line_id: String,
        via_boarding_point_id: String,
        via_alighting_point_id: String,
        line_label: String,
        headsign: String,
    },
    #[serde(rename = "youbike")]
    YouBike {
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
        mode: TravelMode::Walking,
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
        travel_modes: vec![TravelMode::Walking],
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

pub fn selected_modes_as_strings(modes: &[SelectedMode]) -> Vec<&'static str> {
    modes
        .iter()
        .map(|mode| match mode {
            SelectedMode::Bus => "bus",
            SelectedMode::Metro => "metro",
            SelectedMode::YouBike => "youbike",
            SelectedMode::Unknown => "unknown",
        })
        .collect()
}
