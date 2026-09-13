//! Reconstruction of surviving graph paths into canonical journey results.

use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::{
    api::{PlanningRequest, RequestCoordinate, normalize_to_taipei},
    domain::{
        journey::{
            EvidenceType, Geometry, Guidance, JourneyClass, JourneyMode, JourneyOption, Leg,
            LegDetails, LiveStatusEvidence, LiveStatusKind, LiveStatusSummary, Place,
            PlanningError, RankingReason, Transfer,
        },
        ranking::{BikeActionEvidence, CandidateJourney, TransferEvidence, TransferKind},
        realtime::FreshnessState,
        transit::{TransitMode, TransitRide},
    },
    providers::realtime::{NormalizedRealtimeValue, RealtimeValue},
    street::{
        Coordinate, DetailedPath, DetailedPathQuery, Maneuver, ManeuverKind, ProviderError,
        RoutingConstraints, RoutingTimeKind, StreetTravelMode,
    },
};

use super::{
    enrich, graph, search,
    service::{GeneratedCandidate, PlanningContext, RoutingService, is_transient_provider_error},
};

#[derive(Clone)]
struct EffectiveRide {
    ride: TransitRide,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
    evidence: Vec<LiveStatusEvidence>,
}

impl RoutingService {
    pub(crate) async fn reconstruct_multimodal_candidate(
        &self,
        request: &PlanningRequest,
        graph: &graph::RouteGraph,
        journey: &search::SearchLabel,
        realtime: &enrich::RealtimeContext,
        context: &PlanningContext,
        direct_walking_path: Option<&DetailedPath>,
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
                graph::RouteEdge::Walk { from, to, metric } => {
                    if matches!(from, graph::RouteNode::TransitStop(_))
                        && matches!(to, graph::RouteNode::TransitStop(_))
                    {
                        if let Some(from_index) = leg_windows.len().checked_sub(1) {
                            pending_transfers.push((from_index, metric.duration_seconds));
                        }
                        current_time += Duration::seconds(i64::from(metric.duration_seconds));
                        continue;
                    }
                    let path = if matches!(
                        (from, to),
                        (graph::RouteNode::Origin, graph::RouteNode::Destination)
                    ) {
                        direct_walking_path.cloned()
                    } else {
                        self.hydrate_street_edge(
                            request,
                            from,
                            to,
                            StreetTravelMode::Walking,
                            context.constraints(current_time, RoutingTimeKind::DepartAt),
                        )
                        .await?
                    };
                    let Some(path) = path else {
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
                graph::RouteEdge::Transit { ride, .. } => {
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
                graph::RouteEdge::Bike {
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
            }
        }
        if legs.is_empty() || !matches!(journey.node, graph::RouteNode::Destination) {
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

    pub(crate) async fn hydrate_street_edge(
        &self,
        request: &PlanningRequest,
        from: &graph::RouteNode,
        to: &graph::RouteNode,
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

    pub(crate) fn node_coordinate(
        &self,
        request: &PlanningRequest,
        node: &graph::RouteNode,
    ) -> Option<Coordinate> {
        let coordinate = match node {
            graph::RouteNode::Origin => request.origin.coordinate,
            graph::RouteNode::Destination => request.destination.coordinate,
            graph::RouteNode::TransitStop(stop_id) => {
                self.transit_network.stop(stop_id)?.coordinate
            }
            graph::RouteNode::BikeShareStation(station_id) => {
                self.transit_network
                    .bike_share_station(station_id)?
                    .coordinate
            }
        };
        Coordinate::try_new(coordinate.latitude, coordinate.longitude).ok()
    }

    pub(crate) fn node_place(
        &self,
        request: &PlanningRequest,
        node: &graph::RouteNode,
    ) -> Option<Place> {
        match node {
            graph::RouteNode::Origin => Some(Place {
                display_name: request.origin.display_text.clone(),
                coordinate: request.origin.coordinate,
                kind: "selected_place",
                via_id: None,
            }),
            graph::RouteNode::Destination => Some(Place {
                display_name: request.destination.display_text.clone(),
                coordinate: request.destination.coordinate,
                kind: "selected_place",
                via_id: None,
            }),
            graph::RouteNode::TransitStop(stop_id) => {
                self.transit_network.stop(stop_id).map(transit_place)
            }
            graph::RouteNode::BikeShareStation(station_id) => self
                .transit_network
                .bike_share_station(station_id)
                .map(bike_place),
        }
    }
}

pub(crate) fn latest_departure_for_path(
    graph: &graph::RouteGraph,
    edge_ids: &[usize],
    latest_arrival: OffsetDateTime,
) -> Option<OffsetDateTime> {
    let mut required_arrival = latest_arrival;
    for edge_id in edge_ids.iter().rev() {
        let edge = graph.edges.get(*edge_id)?;
        match edge {
            graph::RouteEdge::Walk { metric, .. } | graph::RouteEdge::Bike { metric, .. } => {
                required_arrival -= Duration::seconds(i64::from(metric.duration_seconds));
            }
            graph::RouteEdge::Transit { ride, .. } => {
                if ride.arrival > required_arrival {
                    return None;
                }
                required_arrival = ride.departure;
            }
        }
    }
    Some(required_arrival)
}

pub(crate) fn apply_ranked_journey(
    option: &mut JourneyOption,
    ranked: &crate::domain::ranking::RankedJourney,
) {
    option.class = ranked.class;
    option.label = ranked.label;
    option.ranking_reasons = ranked.ranking_reasons.clone();
}

pub(crate) fn assign_result_local_ids(option: &mut JourneyOption, index: usize) {
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
    service: &crate::domain::transit::TransitService,
    boarding: &crate::domain::transit::TransitStop,
    alighting: &crate::domain::transit::TransitStop,
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
        distance_metres: transit_leg_distance_metres(boarding.coordinate, alighting.coordinate),
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
pub(crate) fn bike_leg(
    id: String,
    pickup: &crate::domain::transit::BikeShareStation,
    dropoff: &crate::domain::transit::BikeShareStation,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &DetailedPath,
    pickup_availability: &crate::domain::realtime::BikeAvailability,
    dropoff_availability: &crate::domain::realtime::BikeAvailability,
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

pub(crate) fn transit_place(stop: &crate::domain::transit::TransitStop) -> Place {
    Place {
        display_name: stop.name.clone(),
        coordinate: stop.coordinate,
        kind: "boarding_point",
        via_id: Some(stop.id.as_str().into()),
    }
}

pub(crate) fn bike_place(station: &crate::domain::transit::BikeShareStation) -> Place {
    Place {
        display_name: station.name.clone(),
        coordinate: station.coordinate,
        kind: "bike_share_station",
        via_id: Some(station.id.as_str().into()),
    }
}

pub(crate) fn overview_geometry(legs: &[Leg]) -> Geometry {
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

pub(crate) fn straight_geometry(from: RequestCoordinate, to: RequestCoordinate) -> Geometry {
    let from = Coordinate::try_new(from.latitude, from.longitude).expect("validated coordinate");
    let to = Coordinate::try_new(to.latitude, to.longitude).expect("validated coordinate");
    Geometry {
        encoding: "polyline6",
        points: encode_polyline6(&[from, to]),
    }
}

pub(crate) fn transit_leg_distance_metres(from: RequestCoordinate, to: RequestCoordinate) -> u32 {
    let latitude = ((from.latitude + to.latitude) / 2.0).to_radians();
    let latitude_metres = (to.latitude - from.latitude).abs() * 111_320.0;
    let longitude_metres = (to.longitude - from.longitude).abs() * 111_320.0 * latitude.cos();
    (latitude_metres.hypot(longitude_metres)).round() as u32
}

pub(crate) fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> Option<u32> {
    u32::try_from((to - from).whole_seconds()).ok()
}

pub(crate) fn push_unique_mode(modes: &mut Vec<JourneyMode>, mode: JourneyMode) {
    if !modes.contains(&mode) {
        modes.push(mode);
    }
}

pub(crate) fn transfer_kind(from: Option<TransitMode>, to: Option<TransitMode>) -> TransferKind {
    if from == Some(TransitMode::Metro) && to == Some(TransitMode::Metro) {
        TransferKind::MetroToMetro
    } else {
        TransferKind::BusInvolved
    }
}

pub(crate) fn bike_value(
    status: &NormalizedRealtimeValue,
) -> Option<&crate::domain::realtime::BikeAvailability> {
    match &status.value {
        RealtimeValue::BikeAvailability(value) => Some(value),
        _ => None,
    }
}

pub(crate) fn bike_observed_at(
    availability: &crate::domain::realtime::BikeAvailability,
) -> Option<OffsetDateTime> {
    availability
        .provider_published_at
        .or(availability.source_change_at)
        .or(Some(availability.received_at))
}

pub(crate) fn status_evidence(
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

pub(crate) fn summary_from_evidence(evidence: &[LiveStatusEvidence]) -> LiveStatusSummary {
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

pub(crate) fn short_digest(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("match_{:x}", digest.finalize())[..22].to_owned()
}

pub(crate) fn guidance(maneuvers: &[Maneuver]) -> Vec<Guidance> {
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

pub(crate) fn encode_polyline6(coordinates: &[Coordinate]) -> String {
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

pub(crate) fn encode_polyline_value(value: i64, output: &mut String) {
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

pub(crate) fn decode_polyline6(encoded: &str) -> Option<Vec<Coordinate>> {
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

pub(crate) fn to_street_coordinate(
    coordinate: RequestCoordinate,
) -> Result<Coordinate, PlanningError> {
    Coordinate::try_new(coordinate.latitude, coordinate.longitude).map_err(PlanningError::from)
}

pub(crate) fn taipei_now() -> OffsetDateTime {
    OffsetDateTime::now_utc().to_offset(UtcOffset::from_hms(8, 0, 0).unwrap())
}
