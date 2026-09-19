//! Expand transit candidates into rider-visible walking and transit legs.

use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        journey::{
            Geometry, Guidance, JourneyClass, JourneyMode, Leg, LegDetails, LiveStatusEvidence,
            Place, PlanningError, RankingReason, Transfer,
        },
        ranking::TransferEvidence,
        realtime::FreshnessState,
        transit::TransitMode,
        transit_timing::{TimingBasis, TimingReliability},
    },
    street::{Coordinate, RoutingTimeKind, StreetPath, StreetTravelMode},
};

use super::{
    super::{
        algorithm::JourneySegment,
        augmentation::{
            JourneyAnchorKind, JourneyMovementKind, JourneyPathMovement, JourneyVariant,
            WalkingMovementKind,
        },
        service::{
            PlanningContext, RoutingService, is_global_provider_error, is_transient_provider_error,
        },
    },
    evidence, geometry,
};

#[derive(Clone)]
struct EffectiveRide {
    scheduled_departure: OffsetDateTime,
    scheduled_arrival: OffsetDateTime,
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
    pub(crate) walking_seconds: u32,
    pub(crate) bicycle_seconds: u32,
    pub(crate) bike_share_uses: u32,
    pub(crate) timing_bases: Vec<TimingBasis>,
    pub(crate) timing_reliability: TimingReliability,
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
    variant: &JourneyVariant,
    context: &PlanningContext,
) -> Result<Option<ReconstructedLegs>, PlanningError> {
    let candidate = variant.skeleton().candidate();
    let mut result = ReconstructedLegs {
        legs: Vec::new(),
        transfers: Vec::new(),
        arrival_time: candidate.departure_time,
        live_evidence: Vec::new(),
        required_states: Vec::new(),
        travel_modes: Vec::new(),
        main_service_sequence: Vec::new(),
        transfer_evidence: Vec::new(),
        walking_seconds: 0,
        bicycle_seconds: 0,
        bike_share_uses: 0,
        timing_bases: Vec::new(),
        timing_reliability: TimingReliability::Reliable,
    };
    let origin = service.origin_place(request);
    let destination = service.destination_place(request);
    let mut pending_transfer = None;
    let mut current_stop = candidate.access.stop_id.clone();
    let mut current_anchor = 0;
    let final_anchor = variant.skeleton().anchors().len().saturating_sub(1);
    for step in variant.path() {
        if step.from_anchor != current_anchor || step.to_anchor <= step.from_anchor {
            return Ok(None);
        }
        match &step.movement {
            JourneyPathMovement::Baseline { movement_index } => {
                let movement = variant
                    .skeleton()
                    .movements()
                    .get(*movement_index)
                    .ok_or(PlanningError::Internal)?;
                if movement.from_anchor != step.from_anchor || movement.to_anchor != step.to_anchor
                {
                    return Ok(None);
                }
                match &movement.kind {
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Access,
                        duration_seconds,
                        distance_metres,
                    } => {
                        if current_anchor != 0 {
                            return Ok(None);
                        }
                        let first_stop = service
                            .stop_place(&current_stop)
                            .ok_or(PlanningError::Internal)?;
                        append_hydrated_walking(
                            service,
                            context,
                            &origin,
                            &first_stop,
                            *duration_seconds,
                            *distance_metres,
                            &mut result,
                        )
                        .await?;
                    }
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Transfer,
                        duration_seconds,
                        ..
                    } => {
                        let from = service
                            .stop_place(&current_stop)
                            .ok_or(PlanningError::Internal)?;
                        let to_stop = anchor_boarding_point(variant, step.to_anchor)
                            .ok_or(PlanningError::Internal)?;
                        if to_stop == current_stop {
                            return Ok(None);
                        }
                        let to = service
                            .stop_place(&to_stop)
                            .ok_or(PlanningError::Internal)?;
                        let connector_start = result.legs.len();
                        append_static_walking(&from, &to, *duration_seconds, &mut result);
                        register_pending_transfer(&result, &mut pending_transfer, connector_start);
                        current_stop = to_stop;
                    }
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Egress,
                        duration_seconds,
                        distance_metres,
                    } => {
                        if step.to_anchor != final_anchor {
                            return Ok(None);
                        }
                        let final_stop = service
                            .stop_place(&current_stop)
                            .ok_or(PlanningError::Internal)?;
                        append_hydrated_walking(
                            service,
                            context,
                            &final_stop,
                            &destination,
                            *duration_seconds,
                            *distance_metres,
                            &mut result,
                        )
                        .await?;
                    }
                    JourneyMovementKind::Transit { segment_index } => {
                        let Some(JourneySegment::Transit {
                            trip,
                            board_index,
                            alight_index,
                        }) = candidate.segments.get(*segment_index)
                        else {
                            return Err(PlanningError::Internal);
                        };
                        let board = trip
                            .stop_times
                            .get(*board_index)
                            .ok_or(PlanningError::Internal)?;
                        let alight = trip
                            .stop_times
                            .get(*alight_index)
                            .ok_or(PlanningError::Internal)?;
                        if board.stop_id != current_stop || result.arrival_time > board.departure_at
                        {
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
                        result.legs.push(transit_leg(
                            leg_id.clone(),
                            service_data,
                            boarding,
                            alighting,
                            &EffectiveRide {
                                scheduled_departure: board.departure_at,
                                scheduled_arrival: alight.arrival_at,
                                departure: board.departure_at,
                                arrival: alight.arrival_at,
                                evidence: live,
                            },
                            trip.timing_basis,
                        ));
                        if let Some(pending) = pending_transfer.take() {
                            add_transfer(
                                &mut result,
                                pending,
                                leg_id,
                                service_data.transit_mode,
                                board.departure_at,
                            );
                        }
                        result.arrival_time = alight.arrival_at;
                        result.timing_bases.push(trip.timing_basis);
                        if trip.timing_reliability == TimingReliability::Fallback {
                            result.timing_reliability = TimingReliability::Fallback;
                        }
                        evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Transit);
                        result
                            .main_service_sequence
                            .push(service_data.id.as_str().to_owned());
                        current_stop = alight.stop_id.clone();
                    }
                }
            }
            JourneyPathMovement::BikeShare { shortcut } => {
                if shortcut.from_anchor != step.from_anchor || shortcut.to_anchor != step.to_anchor
                {
                    return Ok(None);
                }
                if matches!(
                    variant.skeleton().anchors()[step.from_anchor].kind,
                    JourneyAnchorKind::Destination { .. }
                ) || matches!(
                    variant.skeleton().anchors()[step.to_anchor].kind,
                    JourneyAnchorKind::Origin { .. }
                ) {
                    return Ok(None);
                }
                let source_stop = anchor_boarding_point(variant, step.from_anchor);
                if source_stop
                    .as_ref()
                    .is_some_and(|source_stop| source_stop != &current_stop)
                {
                    return Ok(None);
                }
                let from = anchor_place(service, request, variant, step.from_anchor)
                    .ok_or(PlanningError::Internal)?;
                let to = anchor_place(service, request, variant, step.to_anchor)
                    .ok_or(PlanningError::Internal)?;
                let connector_start = result.legs.len();
                if !append_bike_share(service, context, &from, &to, shortcut, &mut result).await? {
                    return Ok(None);
                }
                register_pending_transfer(&result, &mut pending_transfer, connector_start);
                if let Some(to_stop) = anchor_boarding_point(variant, step.to_anchor) {
                    current_stop = to_stop;
                }
            }
        }
        current_anchor = step.to_anchor;
    }

    if current_anchor != final_anchor {
        return Ok(None);
    }
    Ok((!result.legs.is_empty()).then_some(result))
}

fn anchor_boarding_point(
    variant: &JourneyVariant,
    anchor: usize,
) -> Option<crate::domain::ids::BoardingPointId> {
    match &variant.skeleton().anchors().get(anchor)?.kind {
        JourneyAnchorKind::BoardingPoint { id } => Some(id.clone()),
        JourneyAnchorKind::Origin { .. } | JourneyAnchorKind::Destination { .. } => None,
    }
}

fn anchor_place(
    service: &RoutingService,
    request: &PlanningRequest,
    variant: &JourneyVariant,
    anchor: usize,
) -> Option<Place> {
    match &variant.skeleton().anchors().get(anchor)?.kind {
        JourneyAnchorKind::Origin { .. } => Some(service.origin_place(request)),
        JourneyAnchorKind::BoardingPoint { id } => service.stop_place(id),
        JourneyAnchorKind::Destination { .. } => Some(service.destination_place(request)),
    }
}

fn register_pending_transfer(
    result: &ReconstructedLegs,
    pending_transfer: &mut Option<PendingTransfer>,
    connector_start: usize,
) {
    let Some(from_leg) = result
        .legs
        .iter()
        .rev()
        .find(|leg| leg.mode == JourneyMode::Transit)
    else {
        return;
    };
    let connector_legs = result.legs[connector_start..].iter();
    let connector_leg_ids = connector_legs
        .clone()
        .map(|leg| leg.id.clone())
        .collect::<Vec<_>>();
    let duration_seconds = connector_legs.map(|leg| leg.duration_seconds).sum();
    let from_mode = match &from_leg.details {
        LegDetails::Transit { transit_mode, .. } => Some(*transit_mode),
        LegDetails::Walking { .. } | LegDetails::BikeShare { .. } => None,
    };
    if let Some(pending) = pending_transfer
        .as_mut()
        .filter(|pending| pending.from_leg_id == from_leg.id)
    {
        pending.duration_seconds = pending.duration_seconds.saturating_add(duration_seconds);
        pending.connector_leg_ids.extend(connector_leg_ids);
    } else {
        *pending_transfer = Some(PendingTransfer {
            from_leg_id: from_leg.id.clone(),
            from_arrival: from_leg.arrival_time,
            duration_seconds,
            connector_leg_ids,
            from_mode,
        });
    }
}

async fn append_bike_share(
    service: &RoutingService,
    context: &PlanningContext,
    from: &Place,
    to: &Place,
    shortcut: &crate::routing::augmentation::BikeShortcut,
    result: &mut ReconstructedLegs,
) -> Result<bool, PlanningError> {
    let Some(pickup) = service.bike_station_place(&shortcut.pickup_station_id) else {
        return Ok(false);
    };
    let Some(dropoff) = service.bike_station_place(&shortcut.dropoff_station_id) else {
        return Ok(false);
    };

    let Some(to_pickup) = strict_bike_path(
        service,
        context,
        from,
        &pickup,
        StreetTravelMode::Walking,
        result.arrival_time,
    )
    .await
    else {
        return Ok(false);
    };
    append_routed_walking(from, &pickup, to_pickup, result);

    let Some(bicycle) = strict_bike_path(
        service,
        context,
        &pickup,
        &dropoff,
        StreetTravelMode::Bicycle,
        result.arrival_time,
    )
    .await
    else {
        return Ok(false);
    };
    append_bicycle(&pickup, &dropoff, bicycle, result);

    let Some(from_dropoff) = strict_bike_path(
        service,
        context,
        &dropoff,
        to,
        StreetTravelMode::Walking,
        result.arrival_time,
    )
    .await
    else {
        return Ok(false);
    };
    append_routed_walking(&dropoff, to, from_dropoff, result);
    result.bike_share_uses = result.bike_share_uses.saturating_add(1);
    Ok(true)
}

async fn strict_bike_path(
    service: &RoutingService,
    context: &PlanningContext,
    from: &Place,
    to: &Place,
    mode: StreetTravelMode,
    departure: OffsetDateTime,
) -> Option<StreetPath> {
    hydrate_path(service, context, from, to, mode, departure)
        .await
        .unwrap_or_default()
}

fn append_routed_walking(
    from: &Place,
    to: &Place,
    path: StreetPath,
    result: &mut ReconstructedLegs,
) {
    let departure = result.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", result.legs.len());
    result
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    result.walking_seconds = result.walking_seconds.saturating_add(path.duration.0);
    result.arrival_time = arrival;
    evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Walking);
}

fn append_bicycle(
    pickup: &Place,
    dropoff: &Place,
    path: StreetPath,
    result: &mut ReconstructedLegs,
) {
    let departure = result.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", result.legs.len());
    result.legs.push(Leg {
        id: leg_id,
        mode: JourneyMode::Bicycle,
        from: pickup.clone(),
        to: dropoff.clone(),
        departure_time: departure,
        arrival_time: arrival,
        scheduled_departure_time: None,
        scheduled_arrival_time: None,
        duration_seconds: path.duration.0,
        distance_metres: path.distance.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: geometry::encode_polyline6(&path.geometry),
        },
        guidance: vec![Guidance {
            code: "bike_share",
            required: true,
            parameters: std::collections::BTreeMap::from([
                (
                    "pickup_station_name".into(),
                    serde_json::json!(pickup.display_name),
                ),
                (
                    "dropoff_station_name".into(),
                    serde_json::json!(dropoff.display_name),
                ),
            ]),
        }],
        details: LegDetails::BikeShare {
            pickup_station_id: pickup.via_id.clone().unwrap_or_default(),
            pickup_station_name: pickup.display_name.clone(),
            dropoff_station_id: dropoff.via_id.clone().unwrap_or_default(),
            dropoff_station_name: dropoff.display_name.clone(),
        },
        live_status: Vec::new(),
        warnings: Vec::new(),
        attribution: path.attribution.clone(),
    });
    result.bicycle_seconds = result.bicycle_seconds.saturating_add(path.duration.0);
    result.arrival_time = arrival;
    evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Bicycle);
}

async fn append_hydrated_walking(
    service: &RoutingService,
    context: &PlanningContext,
    from: &Place,
    to: &Place,
    fallback_seconds: u32,
    fallback_metres: u32,
    result: &mut ReconstructedLegs,
) -> Result<(), PlanningError> {
    let path = hydrated_or_synthetic(
        service,
        context,
        from,
        to,
        StreetTravelMode::Walking,
        result.arrival_time,
        (fallback_seconds, fallback_metres),
    )
    .await?;
    let departure = result.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", result.legs.len());
    result
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    result.walking_seconds = result.walking_seconds.saturating_add(path.duration.0);
    result.arrival_time = arrival;
    evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Walking);
    Ok(())
}

fn append_static_walking(
    from: &Place,
    to: &Place,
    duration_seconds: u32,
    result: &mut ReconstructedLegs,
) {
    let path = synthetic_street_path(from, to, duration_seconds, 0);
    let departure = result.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(duration_seconds));
    let leg_id = format!("candidate_leg_{}", result.legs.len());
    result
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    result.walking_seconds = result.walking_seconds.saturating_add(duration_seconds);
    result.arrival_time = arrival;
    evidence::push_unique_mode(&mut result.travel_modes, JourneyMode::Walking);
}

async fn hydrated_or_synthetic(
    service: &RoutingService,
    context: &PlanningContext,
    from: &Place,
    to: &Place,
    mode: StreetTravelMode,
    departure: OffsetDateTime,
    fallback: (u32, u32),
) -> Result<StreetPath, PlanningError> {
    match hydrate_path(service, context, from, to, mode, departure).await? {
        Some(path) => Ok(path),
        None => Ok(synthetic_street_path(from, to, fallback.0, fallback.1)),
    }
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
        scheduled_departure_time: schedule.then_some(effective.scheduled_departure),
        scheduled_arrival_time: schedule.then_some(effective.scheduled_arrival),
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

pub(crate) fn transit_place(stop: &crate::domain::transit::TransitStop) -> Place {
    Place {
        display_name: stop.name.clone(),
        coordinate: stop.coordinate,
        kind: "boarding_point",
        via_id: Some(stop.id.as_str().into()),
    }
}

pub(crate) fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> Option<u32> {
    u32::try_from((to - from).whole_seconds()).ok()
}
