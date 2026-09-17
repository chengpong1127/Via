//! Expand candidate segments into rider-visible legs.

use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        journey::{
            Geometry, Guidance, JourneyClass, JourneyMode, Leg, LegDetails, LiveStatusEvidence,
            Place, PlanningError, RankingReason, Transfer,
        },
        ranking::{BikeActionEvidence, TransferEvidence},
        realtime::{
            ActionState, BikeAvailability, FreshnessState, NormalizedRealtimeValue, RealtimeValue,
        },
        transit::{TransitMode, TransitRide},
        transit_timing::TimingBasis,
    },
    street::{Coordinate, RoutingTimeKind, StreetPath, StreetTravelMode},
};

use super::{
    super::{
        algorithm::{JourneyCandidate, JourneySegment},
        bike::{EndpointLeg, EndpointPath},
        service::{
            PlanningContext, RoutingService, is_global_provider_error, is_transient_provider_error,
        },
        status::RealtimeContext,
    },
    evidence, geometry,
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
    pub(crate) travel_modes: Vec<JourneyMode>,
    pub(crate) main_service_sequence: Vec<String>,
    pub(crate) transfer_evidence: Vec<TransferEvidence>,
    pub(crate) bike_availability: Option<BikeActionEvidence>,
    pub(crate) walking_seconds: u32,
}

struct PendingTransfer {
    from_leg_id: String,
    from_arrival: OffsetDateTime,
    duration_seconds: u32,
    connector_leg_ids: Vec<String>,
    from_mode: Option<TransitMode>,
}

pub(crate) async fn assemble(
    service: &RoutingService,
    request: &PlanningRequest,
    candidate: &JourneyCandidate,
    realtime: &RealtimeContext,
    context: &PlanningContext,
) -> Result<Option<ReconstructedLegs>, PlanningError> {
    let mut result = ReconstructedLegs {
        legs: Vec::new(),
        transfers: Vec::new(),
        arrival_time: candidate.departure_time,
        live_evidence: Vec::new(),
        required_states: Vec::new(),
        travel_modes: Vec::new(),
        main_service_sequence: Vec::new(),
        transfer_evidence: Vec::new(),
        bike_availability: None,
        walking_seconds: 0,
    };
    let origin = service.origin_place(request);
    let destination = service.destination_place(request);
    let first_stop = service
        .stop_place(&candidate.access.stop_id)
        .ok_or(PlanningError::Internal)?;
    let access_path = candidate.access.path.clone().unwrap_or_else(|| {
        EndpointPath::walking(
            candidate.access.walking_seconds,
            candidate.access.distance_metres,
        )
    });
    if !append_endpoint_path(
        service,
        context,
        &origin,
        &first_stop,
        &access_path,
        &mut result,
        realtime,
    )
    .await?
    {
        return Ok(None);
    }

    let mut pending_transfer = None;
    let mut current_stop = candidate.access.stop_id.clone();
    for segment in &candidate.segments {
        match segment {
            JourneySegment::Transfer { shortcut } => {
                if shortcut.from != current_stop {
                    return Ok(None);
                }
                let from = service
                    .stop_place(&shortcut.from)
                    .ok_or(PlanningError::Internal)?;
                let to = service
                    .stop_place(&shortcut.to)
                    .ok_or(PlanningError::Internal)?;
                let connector_start = result.legs.len();
                if !append_transit_transfer(&from, &to, shortcut.duration_seconds, &mut result) {
                    return Ok(None);
                }
                if let Some(from_leg) = result
                    .legs
                    .iter()
                    .rev()
                    .find(|leg| leg.mode == JourneyMode::Transit)
                {
                    let from_mode = match &from_leg.details {
                        LegDetails::Transit { transit_mode, .. } => Some(*transit_mode),
                        _ => None,
                    };
                    pending_transfer = Some(PendingTransfer {
                        from_leg_id: from_leg.id.clone(),
                        from_arrival: from_leg.arrival_time,
                        duration_seconds: shortcut.duration_seconds,
                        connector_leg_ids: result.legs[connector_start..]
                            .iter()
                            .map(|leg| leg.id.clone())
                            .collect(),
                        from_mode,
                    });
                }
                current_stop = shortcut.to.clone();
            }
            JourneySegment::Transit {
                trip,
                board_index,
                alight_index,
            } => {
                let board = trip
                    .stop_times
                    .get(*board_index)
                    .ok_or(PlanningError::Internal)?;
                let alight = trip
                    .stop_times
                    .get(*alight_index)
                    .ok_or(PlanningError::Internal)?;
                if board.stop_id != current_stop || result.arrival_time > board.departure_at {
                    return Ok(None);
                }
                let service_data = service
                    .transit_network
                    .service(&trip.service_id)
                    .ok_or(PlanningError::Internal)?;
                let boarding = service
                    .transit_network
                    .stop(&board.stop_id)
                    .ok_or(PlanningError::Internal)?;
                let alighting = service
                    .transit_network
                    .stop(&alight.stop_id)
                    .ok_or(PlanningError::Internal)?;
                let live = trip
                    .evidence
                    .iter()
                    .map(evidence::from_realtime_value)
                    .collect::<Vec<_>>();
                result
                    .required_states
                    .extend(trip.evidence.iter().map(|status| status.freshness.state));
                result.live_evidence.extend(live.clone());
                let leg_id = format!("candidate_leg_{}", result.legs.len());
                let effective = EffectiveRide {
                    ride: TransitRide {
                        transit_mode: service_data.transit_mode,
                        service_id: trip.service_id.clone(),
                        trip_id: trip.scheduled_trip_id.clone().unwrap_or_else(|| {
                            crate::domain::ids::ScheduledTripId::new(format!(
                                "effective:{}",
                                trip.id.as_str()
                            ))
                        }),
                        board_stop_id: board.stop_id.clone(),
                        alight_stop_id: alight.stop_id.clone(),
                        scheduled_departure: board.departure_at,
                        scheduled_arrival: alight.arrival_at,
                    },
                    departure: board.departure_at,
                    arrival: alight.arrival_at,
                    evidence: live,
                };
                result.legs.push(transit_leg(
                    leg_id.clone(),
                    service_data,
                    boarding,
                    alighting,
                    &effective,
                    trip.timing_basis,
                ));
                if let Some(pending) = pending_transfer.take() {
                    add_transfer(
                        &mut result,
                        pending,
                        leg_id.clone(),
                        service_data.transit_mode,
                        board.departure_at,
                    );
                }
                result.arrival_time = alight.arrival_at;
                evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Transit);
                result
                    .main_service_sequence
                    .push(service_data.id.as_str().to_owned());
                current_stop = alight.stop_id.clone();
            }
        }
    }

    if candidate.segments.is_empty() {
        return Ok(None);
    }
    let final_stop = service
        .stop_place(&candidate.egress.stop_id)
        .ok_or(PlanningError::Internal)?;
    let egress_path = candidate.egress.path.clone().unwrap_or_else(|| {
        EndpointPath::walking(
            candidate.egress.walking_seconds,
            candidate.egress.distance_metres,
        )
    });
    if !append_endpoint_path(
        service,
        context,
        &final_stop,
        &destination,
        &egress_path,
        &mut result,
        realtime,
    )
    .await?
    {
        return Ok(None);
    }
    if result.legs.is_empty() {
        return Ok(None);
    }
    Ok(Some(result))
}

pub(crate) async fn assemble_direct_endpoint(
    service: &RoutingService,
    request: &PlanningRequest,
    path: &EndpointPath,
    departure: OffsetDateTime,
    realtime: &RealtimeContext,
    context: &PlanningContext,
) -> Result<Option<ReconstructedLegs>, PlanningError> {
    let mut result = ReconstructedLegs {
        legs: Vec::new(),
        transfers: Vec::new(),
        arrival_time: departure,
        live_evidence: Vec::new(),
        required_states: Vec::new(),
        travel_modes: Vec::new(),
        main_service_sequence: Vec::new(),
        transfer_evidence: Vec::new(),
        bike_availability: None,
        walking_seconds: 0,
    };
    let origin = service.origin_place(request);
    let destination = service.destination_place(request);
    if !append_endpoint_path(
        service,
        context,
        &origin,
        &destination,
        path,
        &mut result,
        realtime,
    )
    .await?
    {
        return Ok(None);
    }
    if result.legs.is_empty() {
        return Ok(None);
    }
    Ok(Some(result))
}

fn append_transit_transfer(
    from: &Place,
    to: &Place,
    duration_seconds: u32,
    result: &mut ReconstructedLegs,
) -> bool {
    let walking_path = synthetic_street_path(from, to, duration_seconds, 0);
    let departure = result.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(duration_seconds));
    let leg_id = format!("candidate_leg_{}", result.legs.len());
    result.legs.push(walking_leg(
        leg_id,
        from,
        to,
        departure,
        arrival,
        &walking_path,
    ));
    result.walking_seconds = result.walking_seconds.saturating_add(duration_seconds);
    result.arrival_time = arrival;
    evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Walking);
    true
}

async fn append_endpoint_path(
    service: &RoutingService,
    context: &PlanningContext,
    from: &Place,
    to: &Place,
    path: &EndpointPath,
    result: &mut ReconstructedLegs,
    realtime: &RealtimeContext,
) -> Result<bool, PlanningError> {
    let bike_index = path
        .legs
        .iter()
        .position(|leg| matches!(leg, EndpointLeg::BikeShare { .. }));
    let mut current = from.clone();
    for (index, leg) in path.legs.iter().enumerate() {
        let next = match leg {
            EndpointLeg::BikeShare {
                pickup_station_id,
                return_station_id,
                ..
            } => (
                service
                    .station_place(pickup_station_id)
                    .ok_or(PlanningError::Internal)?,
                service
                    .station_place(return_station_id)
                    .ok_or(PlanningError::Internal)?,
            ),
            EndpointLeg::Walk { .. } if bike_index.is_some_and(|bike| index < bike) => (
                current.clone(),
                service
                    .station_place(match path.legs.get(bike_index.unwrap()) {
                        Some(EndpointLeg::BikeShare {
                            pickup_station_id, ..
                        }) => pickup_station_id,
                        _ => unreachable!(),
                    })
                    .ok_or(PlanningError::Internal)?,
            ),
            EndpointLeg::Walk { .. } => (current.clone(), to.clone()),
        };
        let mode = match leg {
            EndpointLeg::Walk { .. } => StreetTravelMode::Walking,
            EndpointLeg::BikeShare { .. } => StreetTravelMode::Bicycle,
        };
        let path = match hydrate_path(
            service,
            context,
            &next.0,
            &next.1,
            mode,
            result.arrival_time,
        )
        .await?
        {
            Some(path) => path,
            None if path.legs.len() == 1 && mode == StreetTravelMode::Walking => {
                let EndpointLeg::Walk {
                    duration_seconds,
                    distance_metres,
                } = &path.legs[0]
                else {
                    unreachable!();
                };
                synthetic_street_path(&next.0, &next.1, *duration_seconds, *distance_metres)
            }
            None => return Ok(false),
        };
        let departure = result.arrival_time;
        let arrival = departure + Duration::seconds(i64::from(path.duration.0));
        let leg_id = format!("candidate_leg_{}", result.legs.len());
        match leg {
            EndpointLeg::Walk { .. } => {
                result.legs.push(walking_leg(
                    leg_id, &next.0, &next.1, departure, arrival, &path,
                ));
                result.walking_seconds = result.walking_seconds.saturating_add(path.duration.0);
                evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Walking);
            }
            EndpointLeg::BikeShare {
                pickup_station_id,
                return_station_id,
                ..
            } => {
                let (pickup, pickup_live) = effective_bike_availability(
                    realtime.bike(pickup_station_id),
                    pickup_station_id,
                    result.arrival_time,
                );
                let (return_station, return_live) = effective_bike_availability(
                    realtime.bike(return_station_id),
                    return_station_id,
                    result.arrival_time,
                );
                let live = pickup_live
                    .into_iter()
                    .chain(return_live)
                    .collect::<Vec<_>>();
                result.live_evidence.extend(live.clone());
                if result.bike_availability.is_none() {
                    result.bike_availability = Some(BikeActionEvidence {
                        pickup_state: pickup.rental_state,
                        return_state: return_station.return_state,
                        available_bikes: pickup.available_bikes,
                        available_return_slots: return_station.available_return_slots,
                    });
                }
                let mut leg = bike_leg(
                    &next.0,
                    &next.1,
                    departure,
                    &path,
                    &pickup,
                    &return_station,
                    live,
                );
                leg.id = leg_id;
                result.legs.push(leg);
                evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::BikeShare);
                result.main_service_sequence.push(format!(
                    "bike_share:{}:{}",
                    pickup_station_id.as_str(),
                    return_station_id.as_str()
                ));
            }
        }
        result.arrival_time = arrival;
        current = next.1;
    }
    Ok(true)
}

fn synthetic_street_path(
    from: &Place,
    to: &Place,
    duration_seconds: u32,
    distance_metres: u32,
) -> StreetPath {
    StreetPath {
        duration: crate::street::Seconds(duration_seconds),
        distance: crate::street::Metres(distance_metres),
        geometry: vec![
            Coordinate::try_new(from.coordinate.latitude, from.coordinate.longitude)
                .expect("validated place coordinate"),
            Coordinate::try_new(to.coordinate.latitude, to.coordinate.longitude)
                .expect("validated place coordinate"),
        ],
        maneuvers: vec![
            crate::street::Maneuver::depart(0),
            crate::street::Maneuver::arrive(1),
        ],
        attribution: Vec::new(),
    }
}

fn effective_bike_availability(
    status: Option<&NormalizedRealtimeValue>,
    station_id: &crate::domain::ids::BikeShareStationId,
    evaluated_at: OffsetDateTime,
) -> (BikeAvailability, Option<LiveStatusEvidence>) {
    let Some(status) = status else {
        return (unknown_bike_availability(station_id, evaluated_at), None);
    };
    let RealtimeValue::BikeAvailability(availability) = &status.value else {
        return (unknown_bike_availability(station_id, evaluated_at), None);
    };
    let evidence = Some(evidence::from_realtime_value(status));
    if status.freshness.state == FreshnessState::Fresh {
        (availability.clone(), evidence)
    } else {
        (
            unknown_bike_availability(station_id, evaluated_at),
            evidence,
        )
    }
}

fn unknown_bike_availability(
    station_id: &crate::domain::ids::BikeShareStationId,
    received_at: OffsetDateTime,
) -> BikeAvailability {
    BikeAvailability {
        station_id: station_id.clone(),
        rental_state: ActionState::Unknown,
        return_state: ActionState::Unknown,
        available_bikes: None,
        available_return_slots: None,
        source_change_at: None,
        provider_published_at: None,
        received_at,
    }
}

async fn hydrate_path(
    service: &RoutingService,
    context: &PlanningContext,
    from: &Place,
    to: &Place,
    mode: StreetTravelMode,
    departure: OffsetDateTime,
) -> Result<Option<StreetPath>, PlanningError> {
    let from = Coordinate::try_new(from.coordinate.latitude, from.coordinate.longitude)?;
    let to = Coordinate::try_new(to.coordinate.latitude, to.coordinate.longitude)?;
    let constraints = context.constraints(departure, RoutingTimeKind::DepartAt);
    match service.street_path(mode, from, to, constraints).await {
        super::super::service::PathAttempt::Found(path) => Ok(Some(path)),
        super::super::service::PathAttempt::Unreachable => Ok(None),
        super::super::service::PathAttempt::Failed(error)
            if is_transient_provider_error(&error) =>
        {
            Err(PlanningError::TemporarilyUnavailable)
        }
        super::super::service::PathAttempt::Failed(error) if is_global_provider_error(&error) => {
            Err(PlanningError::Internal)
        }
        super::super::service::PathAttempt::Failed(_) => Err(PlanningError::Internal),
    }
}

fn add_transfer(
    result: &mut ReconstructedLegs,
    pending: PendingTransfer,
    to_leg_id: String,
    to_mode: TransitMode,
    departure: OffsetDateTime,
) {
    let available_seconds = seconds_between(pending.from_arrival, departure).unwrap_or(0);
    let kind = evidence::transfer_kind(pending.from_mode, Some(to_mode));
    result.transfer_evidence.push(TransferEvidence {
        kind,
        physical_seconds: pending.duration_seconds,
        available_seconds,
    });
    result.transfers.push(Transfer {
        from_leg_id: pending.from_leg_id,
        to_leg_id,
        connector_leg_ids: pending.connector_leg_ids,
        duration_seconds: pending.duration_seconds,
        available_seconds,
        class: if available_seconds >= pending.duration_seconds {
            JourneyClass::Reliable
        } else {
            JourneyClass::Feasible
        },
        reasons: if available_seconds >= pending.duration_seconds {
            Vec::new()
        } else {
            vec![RankingReason::TightTransferMargin]
        },
        geometry: None,
        guidance: Vec::new(),
        warnings: Vec::new(),
    });
}

pub(crate) fn direct_walking_leg(
    service: &RoutingService,
    request: &PlanningRequest,
    path: &StreetPath,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
) -> Result<Leg, PlanningError> {
    Ok(walking_leg(
        "candidate_leg_0".into(),
        &service.origin_place(request),
        &service.destination_place(request),
        departure,
        arrival,
        path,
    ))
}

fn walking_leg(
    id: String,
    from: &Place,
    to: &Place,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &StreetPath,
) -> Leg {
    Leg {
        id,
        mode: JourneyMode::Walking,
        from: from.clone(),
        to: to.clone(),
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

fn transit_leg(
    id: String,
    service: &crate::domain::transit::TransitService,
    boarding: &crate::domain::transit::TransitStop,
    alighting: &crate::domain::transit::TransitStop,
    effective: &EffectiveRide,
    timing_basis: TimingBasis,
) -> Leg {
    let guidance_code = match service.transit_mode {
        TransitMode::Bus => "board_bus",
        TransitMode::Metro => "board_metro",
        TransitMode::Rail => "board_rail",
        TransitMode::HighSpeedRail => "board_high_speed_rail",
    };
    let schedule = matches!(timing_basis, TimingBasis::Schedule);
    Leg {
        id,
        mode: JourneyMode::Transit,
        from: transit_place(boarding),
        to: transit_place(alighting),
        departure_time: effective.departure,
        arrival_time: effective.arrival,
        scheduled_departure_time: schedule.then_some(effective.ride.scheduled_departure),
        scheduled_arrival_time: schedule.then_some(effective.ride.scheduled_arrival),
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
        details: LegDetails::Transit {
            transit_mode: service.transit_mode,
            via_service_id: service.id.as_str().into(),
            via_boarding_point_id: boarding.id.as_str().into(),
            via_alighting_point_id: alighting.id.as_str().into(),
            service_label: service.label.clone(),
            headsign: service.headsign.clone(),
            stop_count: None,
        },
        live_status: effective.evidence.clone(),
        warnings: Vec::new(),
        attribution: Vec::new(),
    }
}

fn bike_leg(
    from: &Place,
    to: &Place,
    departure_time: OffsetDateTime,
    path: &StreetPath,
    pickup: &crate::domain::realtime::BikeAvailability,
    return_station: &crate::domain::realtime::BikeAvailability,
    evidence: Vec<LiveStatusEvidence>,
) -> Leg {
    Leg {
        id: String::new(),
        mode: JourneyMode::BikeShare,
        from: from.clone(),
        to: to.clone(),
        departure_time,
        arrival_time: departure_time + Duration::seconds(i64::from(path.duration.0)),
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
            parameters: std::collections::BTreeMap::new(),
        }],
        details: LegDetails::BikeShare {
            via_pickup_station_id: pickup.station_id.as_str().into(),
            via_return_station_id: return_station.station_id.as_str().into(),
            pickup_state: pickup.rental_state,
            return_state: return_station.return_state,
            available_bikes: pickup.available_bikes,
            available_return_slots: return_station.available_return_slots,
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
