//! Build rider-visible legs, transfer windows, and timing facts from graph edges.

use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        journey::{
            Geometry, Guidance, JourneyClass, JourneyMode, Leg, LegDetails, LiveStatusEvidence,
            Place, PlanningError, RankingReason, Transfer,
        },
        ranking::{BikeActionEvidence, TransferEvidence},
        realtime::FreshnessState,
        transit::{TransitMode, TransitRide},
    },
    street::{RoutingTimeKind, StreetPath, StreetTravelMode},
};

use super::{
    super::{
        enrich, search,
        service::{PlanningContext, RoutingService},
    },
    evidence, geometry, graph,
};

#[derive(Clone)]
struct EffectiveRide {
    ride: TransitRide,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
    evidence: Vec<LiveStatusEvidence>,
}

pub(crate) struct ReconstructedLegs {
    pub(crate) legs: Vec<Leg>,
    pub(crate) transfers: Vec<Transfer>,
    pub(crate) arrival_time: OffsetDateTime,
    pub(crate) live_evidence: Vec<LiveStatusEvidence>,
    pub(crate) required_states: Vec<FreshnessState>,
    pub(crate) schedule_only: bool,
    pub(crate) travel_modes: Vec<JourneyMode>,
    pub(crate) main_service_sequence: Vec<String>,
    pub(crate) transfer_evidence: Vec<TransferEvidence>,
    pub(crate) bike_availability: Option<BikeActionEvidence>,
    pub(crate) walking_seconds: u32,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn assemble(
    service: &RoutingService,
    request: &PlanningRequest,
    route_graph: &graph::RouteGraph,
    journey: &search::SearchLabel,
    realtime: &enrich::RealtimeContext,
    context: &PlanningContext,
    direct_walking_path: Option<&StreetPath>,
    departure_time: OffsetDateTime,
) -> Result<Option<ReconstructedLegs>, PlanningError> {
    let mut result = ReconstructedLegs {
        legs: Vec::new(),
        transfers: Vec::new(),
        arrival_time: departure_time,
        live_evidence: Vec::new(),
        required_states: Vec::new(),
        schedule_only: journey.schedule_only,
        travel_modes: Vec::new(),
        main_service_sequence: Vec::new(),
        transfer_evidence: Vec::new(),
        bike_availability: None,
        walking_seconds: 0,
    };
    let mut pending_transfers = Vec::<(usize, u32)>::new();
    let mut leg_windows = Vec::<(String, OffsetDateTime, OffsetDateTime)>::new();
    let mut leg_transit_modes = Vec::<Option<TransitMode>>::new();

    for edge_id in &journey.edge_ids {
        let Some(edge) = route_graph.edges.get(*edge_id) else {
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
                    result.arrival_time += Duration::seconds(i64::from(metric.duration_seconds));
                    continue;
                }
                let path = if matches!(
                    (from, to),
                    (graph::RouteNode::Origin, graph::RouteNode::Destination)
                ) {
                    direct_walking_path.cloned()
                } else {
                    service
                        .hydrate_street_edge(
                            request,
                            from,
                            to,
                            StreetTravelMode::Walking,
                            context.constraints(result.arrival_time, RoutingTimeKind::DepartAt),
                        )
                        .await?
                };
                let Some(path) = path else {
                    return Ok(None);
                };
                let Some(from_place) = service.node_place(request, from) else {
                    return Ok(None);
                };
                let Some(to_place) = service.node_place(request, to) else {
                    return Ok(None);
                };
                let departure = result.arrival_time;
                let arrival = departure + Duration::seconds(i64::from(path.duration.0));
                let leg_id = format!("candidate_leg_{}", result.legs.len());
                result.legs.push(walking_leg(
                    leg_id.clone(),
                    &from_place,
                    &to_place,
                    departure,
                    arrival,
                    &path,
                ));
                leg_transit_modes.push(None);
                leg_windows.push((leg_id, departure, arrival));
                complete_pending_transfers(
                    &mut pending_transfers,
                    &leg_windows,
                    &leg_transit_modes,
                    &mut result.transfers,
                    &mut result.transfer_evidence,
                );
                result.arrival_time = arrival;
                result.walking_seconds = result.walking_seconds.saturating_add(path.duration.0);
                evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Walking);
            }
            graph::RouteEdge::Transit { ride, .. } => {
                if result.arrival_time > ride.departure {
                    return Ok(None);
                }
                let service_data = service
                    .transit_network
                    .service(&ride.ride.service_id)
                    .ok_or(PlanningError::Internal)?;
                let boarding = service
                    .transit_network
                    .stop(&ride.ride.board_stop_id)
                    .ok_or(PlanningError::Internal)?;
                let alighting = service
                    .transit_network
                    .stop(&ride.ride.alight_stop_id)
                    .ok_or(PlanningError::Internal)?;
                let live = ride
                    .evidence
                    .iter()
                    .map(evidence::from_realtime_value)
                    .collect::<Vec<_>>();
                result
                    .required_states
                    .extend(ride.required_states.iter().copied());
                result.live_evidence.extend(live.clone());
                result.schedule_only |= ride.schedule_only;
                let effective = EffectiveRide {
                    ride: ride.ride.clone(),
                    departure: ride.departure,
                    arrival: ride.arrival,
                    evidence: live,
                };
                let leg_id = format!("candidate_leg_{}", result.legs.len());
                result.legs.push(transit_leg(
                    leg_id.clone(),
                    service_data,
                    boarding,
                    alighting,
                    &effective,
                ));
                leg_transit_modes.push(Some(service_data.transit_mode));
                leg_windows.push((leg_id, ride.departure, ride.arrival));
                complete_pending_transfers(
                    &mut pending_transfers,
                    &leg_windows,
                    &leg_transit_modes,
                    &mut result.transfers,
                    &mut result.transfer_evidence,
                );
                result.arrival_time = ride.arrival;
                evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Transit);
                result
                    .main_service_sequence
                    .push(service_data.id.as_str().to_owned());
            }
            graph::RouteEdge::Bike {
                from,
                to,
                pickup_station_id,
                return_station_id,
                availability,
                ..
            } => {
                let Some(path) = service
                    .hydrate_street_edge(
                        request,
                        from,
                        to,
                        StreetTravelMode::Bicycle,
                        context.constraints(result.arrival_time, RoutingTimeKind::DepartAt),
                    )
                    .await?
                else {
                    return Ok(None);
                };
                let departure = result.arrival_time;
                let arrival = departure + Duration::seconds(i64::from(path.duration.0));
                let pickup = service
                    .transit_network
                    .bike_share_station(pickup_station_id)
                    .ok_or(PlanningError::Internal)?;
                let return_station = service
                    .transit_network
                    .bike_share_station(return_station_id)
                    .ok_or(PlanningError::Internal)?;
                let bike_evidence = bike_evidence(
                    realtime,
                    pickup_station_id,
                    return_station_id,
                    &mut result.live_evidence,
                    &mut result.required_states,
                );
                result.bike_availability = Some(BikeActionEvidence {
                    pickup_state: availability.pickup.rental_state,
                    return_state: availability.return_station.return_state,
                    available_bikes: availability.pickup.available_bikes,
                    available_return_slots: availability.return_station.available_return_slots,
                });
                let leg_id = format!("candidate_leg_{}", result.legs.len());
                result.legs.push(bike_leg(
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
                result.arrival_time = arrival;
                evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::BikeShare);
                result.main_service_sequence.push(format!(
                    "bike_share:{}:{}",
                    pickup_station_id.as_str(),
                    return_station_id.as_str()
                ));
            }
        }
    }

    if result.legs.is_empty() {
        return Ok(None);
    }
    Ok(Some(result))
}

fn bike_evidence(
    realtime: &enrich::RealtimeContext,
    pickup_station_id: &crate::domain::ids::BikeShareStationId,
    return_station_id: &crate::domain::ids::BikeShareStationId,
    live_evidence: &mut Vec<LiveStatusEvidence>,
    required_states: &mut Vec<FreshnessState>,
) -> Vec<LiveStatusEvidence> {
    [pickup_station_id, return_station_id]
        .into_iter()
        .filter_map(|station_id| realtime.bike(station_id))
        .map(|status| {
            let evidence = evidence::from_realtime_value(status);
            live_evidence.push(evidence.clone());
            required_states.push(status.freshness.state);
            evidence
        })
        .collect()
}

fn complete_pending_transfers(
    pending: &mut Vec<(usize, u32)>,
    leg_windows: &[(String, OffsetDateTime, OffsetDateTime)],
    leg_transit_modes: &[Option<TransitMode>],
    transfers: &mut Vec<Transfer>,
    transfer_evidence: &mut Vec<TransferEvidence>,
) {
    for (from_index, duration_seconds) in pending.drain(..) {
        let Some((from_id, _, from_arrival)) = leg_windows.get(from_index) else {
            continue;
        };
        let Some((to_id, to_departure, _)) = leg_windows.last() else {
            continue;
        };
        let available_seconds = seconds_between(*from_arrival, *to_departure).unwrap_or(0);
        let kind = evidence::transfer_kind(
            leg_transit_modes.get(from_index).copied().flatten(),
            leg_transit_modes.last().copied().flatten(),
        );
        transfer_evidence.push(TransferEvidence {
            kind,
            physical_seconds: duration_seconds,
            available_seconds,
        });
        let reliable = available_seconds >= duration_seconds;
        transfers.push(Transfer {
            from_leg_id: from_id.clone(),
            to_leg_id: to_id.clone(),
            connector_leg_ids: Vec::new(),
            duration_seconds,
            available_seconds,
            class: if reliable {
                JourneyClass::Reliable
            } else {
                JourneyClass::Feasible
            },
            reasons: if reliable {
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

fn walking_leg(
    id: String,
    from: &impl PlaceLike,
    to: &impl PlaceLike,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &StreetPath,
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
            points: geometry::encode_polyline6(&path.geometry),
        },
        guidance: geometry::guidance(&path.maneuvers),
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
        mode: JourneyMode::Transit,
        from: transit_place(boarding),
        to: transit_place(alighting),
        departure_time: effective.departure,
        arrival_time: effective.arrival,
        scheduled_departure_time,
        scheduled_arrival_time,
        duration_seconds: seconds_between(effective.departure, effective.arrival).unwrap_or(0),
        distance_metres: geometry::transit_leg_distance_metres(
            boarding.coordinate,
            alighting.coordinate,
        ),
        geometry: geometry::straight_geometry(boarding.coordinate, alighting.coordinate),
        guidance: vec![Guidance {
            code: guidance_code,
            required: true,
            parameters: std::collections::BTreeMap::from([
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
    pickup: &crate::domain::transit::BikeShareStation,
    dropoff: &crate::domain::transit::BikeShareStation,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &StreetPath,
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
            points: geometry::encode_polyline6(&path.geometry),
        },
        guidance: vec![Guidance {
            code: "rent_bike_share",
            required: true,
            parameters: std::collections::BTreeMap::from([
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

pub(crate) fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> Option<u32> {
    u32::try_from((to - from).whole_seconds()).ok()
}
