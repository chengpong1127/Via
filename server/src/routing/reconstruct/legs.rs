use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        ids::EffectiveTripId,
        journey::{
            Geometry, Guidance, JourneyClass, Leg, LegDetails, LegMode, LiveStatusEvidence, Place,
            PlanningError, RankingReason, Transfer,
        },
        ranking::TransferEvidence,
        realtime::FreshnessState,
        transit::TransitMode,
        transit_timing::{TimingBasis, TimingReliability},
    },
    street::{Coordinate, ProviderFailureClass, RoutingTimeKind, StreetPath, StreetTravelMode},
};

use super::{
    super::{
        path::{
            JourneyAnchorKind, JourneyMovementKind, JourneyPathMovement, JourneyPathStep,
            JourneyVariant, WalkingMovementKind,
        },
        service::{PlanningContext, RoutingService},
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
    pub(crate) travel_modes: Vec<LegMode>,
    pub(crate) main_pattern_sequence: Vec<String>,
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

struct PreviousTransit {
    leg_id: String,
    trip_id: EffectiveTripId,
    arrival: OffsetDateTime,
    mode: TransitMode,
}

pub(crate) async fn assemble(
    service: &RoutingService,
    request: &PlanningRequest,
    variant: &JourneyVariant,
    context: &PlanningContext,
) -> Result<Option<ReconstructedLegs>, PlanningError> {
    let mut result = ReconstructedLegs {
        legs: Vec::new(),
        transfers: Vec::new(),
        arrival_time: variant.skeleton().departure_at(),
        live_evidence: Vec::new(),
        required_states: Vec::new(),
        travel_modes: Vec::new(),
        main_pattern_sequence: Vec::new(),
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
    let mut previous_transit: Option<PreviousTransit> = None;
    let mut current_stop = None;
    let mut current_anchor = 0;
    let final_anchor = variant.skeleton().anchors().len().saturating_sub(1);
    let steps = variant.path();
    let mut skip_next_step = false;
    for (step_index, step) in steps.iter().enumerate() {
        context.ensure_before_deadline()?;
        if skip_next_step {
            skip_next_step = false;
            continue;
        }
        if step.from_anchor != current_anchor || step.to_anchor <= step.from_anchor {
            return Ok(None);
        }
        if let Some((from_stop, egress_stop, duration_seconds)) =
            terminal_surface_transfer_egress(variant, steps, step_index, final_anchor)
        {
            if current_stop.as_ref() != Some(&from_stop) {
                return Ok(None);
            }
            let from = service
                .stop_place(&from_stop)
                .ok_or(PlanningError::Internal)?;
            append_hydrated_walking(
                service,
                context,
                &from,
                &destination,
                duration_seconds,
                0,
                &mut result,
            )
            .await?;
            current_stop = Some(egress_stop);
            current_anchor = steps[step_index + 1].to_anchor;
            skip_next_step = true;
            continue;
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
                            .stop_place(
                                &anchor_boarding_point(variant, step.to_anchor)
                                    .ok_or(PlanningError::Internal)?,
                            )
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
                        current_stop = anchor_boarding_point(variant, step.to_anchor);
                    }
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Transfer,
                        duration_seconds,
                        distance_metres,
                    } => {
                        let from_stop = current_stop
                            .clone()
                            .or_else(|| anchor_boarding_point(variant, step.from_anchor))
                            .ok_or(PlanningError::Internal)?;
                        let from = service
                            .stop_place(&from_stop)
                            .ok_or(PlanningError::Internal)?;
                        let to_stop = anchor_boarding_point(variant, step.to_anchor)
                            .ok_or(PlanningError::Internal)?;
                        if Some(to_stop.clone()) == current_stop {
                            return Ok(None);
                        }
                        let to = service
                            .stop_place(&to_stop)
                            .ok_or(PlanningError::Internal)?;
                        let connector_start = result.legs.len();
                        append_static_walking(
                            &from,
                            &to,
                            *duration_seconds,
                            *distance_metres,
                            &mut result,
                        );
                        register_pending_transfer(&result, &mut pending_transfer, connector_start);
                        current_stop = Some(to_stop);
                    }
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Egress,
                        duration_seconds,
                        distance_metres,
                    } => {
                        if step.to_anchor != final_anchor {
                            return Ok(None);
                        }
                        let final_stop_id = current_stop
                            .clone()
                            .or_else(|| anchor_boarding_point(variant, step.from_anchor))
                            .ok_or(PlanningError::Internal)?;
                        let final_stop = service
                            .stop_place(&final_stop_id)
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
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Direct,
                        ..
                    } => {
                        if current_anchor != 0 || step.to_anchor != final_anchor {
                            return Ok(None);
                        }
                        let Some(path) = movement.street_path.as_deref() else {
                            return Err(PlanningError::Internal);
                        };
                        append_routed_walking(&origin, &destination, path.clone(), &mut result);
                    }
                    JourneyMovementKind::Transit {
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
                        if Some(board.stop_id.clone()) != current_stop
                            || result.arrival_time > board.departure_at
                        {
                            return Ok(None);
                        }
                        let pattern_data = service
                            .transit_runtime
                            .pattern(&trip.pattern_id)
                            .ok_or(PlanningError::Internal)?;
                        let route_data = service.transit_runtime.route_of(pattern_data);
                        let boarding = service
                            .transit_runtime
                            .stop(&board.stop_id)
                            .ok_or(PlanningError::Internal)?;
                        let alighting = service
                            .transit_runtime
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
                            pattern_data,
                            route_data,
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
                                leg_id.clone(),
                                route_data.mode,
                                board.departure_at,
                            );
                        } else if let Some(previous) = &previous_transit
                            && previous.trip_id != trip.id
                        {
                            add_transfer(
                                &mut result,
                                PendingTransfer {
                                    from_leg_id: previous.leg_id.clone(),
                                    from_arrival: previous.arrival,
                                    duration_seconds: service
                                        .transit_runtime
                                        .same_stop_transfer_seconds(),
                                    connector_leg_ids: Vec::new(),
                                    from_mode: Some(previous.mode),
                                },
                                leg_id.clone(),
                                route_data.mode,
                                board.departure_at,
                            );
                        }
                        previous_transit = Some(PreviousTransit {
                            leg_id,
                            trip_id: trip.id.clone(),
                            arrival: alight.arrival_at,
                            mode: route_data.mode,
                        });
                        result.arrival_time = alight.arrival_at;
                        result.timing_bases.push(trip.timing_basis);
                        if trip.timing_reliability == TimingReliability::Fallback {
                            result.timing_reliability = TimingReliability::Fallback;
                        }
                        evidence::push_unique_mode(&mut result.travel_modes, LegMode::Transit);
                        result
                            .main_pattern_sequence
                            .push(pattern_data.id.as_str().to_owned());
                        current_stop = Some(alight.stop_id.clone());
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
                    JourneyAnchorKind::Destination
                ) || matches!(
                    variant.skeleton().anchors()[step.to_anchor].kind,
                    JourneyAnchorKind::Origin
                ) {
                    return Ok(None);
                }
                let source_stop = anchor_boarding_point(variant, step.from_anchor);
                if source_stop
                    .as_ref()
                    .is_some_and(|source_stop| current_stop.as_ref() != Some(source_stop))
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
                    current_stop = Some(to_stop);
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

fn terminal_surface_transfer_egress(
    variant: &JourneyVariant,
    steps: &[JourneyPathStep],
    index: usize,
    final_anchor: usize,
) -> Option<(
    crate::domain::ids::BoardingPointId,
    crate::domain::ids::BoardingPointId,
    u32,
)> {
    let transfer_step = steps.get(index)?;
    let egress_step = steps.get(index + 1)?;
    let JourneyPathMovement::Baseline {
        movement_index: transfer_index,
    } = &transfer_step.movement
    else {
        return None;
    };
    let JourneyPathMovement::Baseline {
        movement_index: egress_index,
    } = &egress_step.movement
    else {
        return None;
    };
    let transfer_movement = variant.skeleton().movements().get(*transfer_index)?;
    let egress_movement = variant.skeleton().movements().get(*egress_index)?;
    let JourneyMovementKind::Walking {
        kind: WalkingMovementKind::Transfer,
        duration_seconds: transfer_seconds,
        ..
    } = &transfer_movement.kind
    else {
        return None;
    };
    let JourneyMovementKind::Walking {
        kind: WalkingMovementKind::Egress,
        duration_seconds: egress_seconds,
        ..
    } = &egress_movement.kind
    else {
        return None;
    };
    if transfer_step.to_anchor != egress_step.from_anchor || egress_step.to_anchor != final_anchor {
        return None;
    }
    Some((
        anchor_boarding_point(variant, transfer_step.from_anchor)?,
        anchor_boarding_point(variant, transfer_step.to_anchor)?,
        (*transfer_seconds).saturating_add(*egress_seconds),
    ))
}

fn anchor_boarding_point(
    variant: &JourneyVariant,
    anchor: usize,
) -> Option<crate::domain::ids::BoardingPointId> {
    match &variant.skeleton().anchors().get(anchor)?.kind {
        JourneyAnchorKind::BoardingPoint { id } => Some(id.clone()),
        JourneyAnchorKind::Origin | JourneyAnchorKind::Destination => None,
    }
}

fn anchor_place(
    service: &RoutingService,
    request: &PlanningRequest,
    variant: &JourneyVariant,
    anchor: usize,
) -> Option<Place> {
    match &variant.skeleton().anchors().get(anchor)?.kind {
        JourneyAnchorKind::Origin => Some(service.origin_place(request)),
        JourneyAnchorKind::BoardingPoint { id } => service.stop_place(id),
        JourneyAnchorKind::Destination => Some(service.destination_place(request)),
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
        .find(|leg| leg.mode == LegMode::Transit)
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
    shortcut: &crate::routing::BikeShortcut,
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
    evidence::push_unique_mode(&mut result.travel_modes, LegMode::Walking);
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
        mode: LegMode::Bicycle,
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
    evidence::push_unique_mode(&mut result.travel_modes, LegMode::Bicycle);
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
    evidence::push_unique_mode(&mut result.travel_modes, LegMode::Walking);
    Ok(())
}

fn append_static_walking(
    from: &Place,
    to: &Place,
    duration_seconds: u32,
    distance_metres: u32,
    result: &mut ReconstructedLegs,
) {
    let path = synthetic_street_path(from, to, duration_seconds, distance_metres);
    let departure = result.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(duration_seconds));
    let leg_id = format!("candidate_leg_{}", result.legs.len());
    result
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    result.walking_seconds = result.walking_seconds.saturating_add(duration_seconds);
    result.arrival_time = arrival;
    evidence::push_unique_mode(&mut result.travel_modes, LegMode::Walking);
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
    let from = Coordinate::try_from(from.coordinate).map_err(|_| PlanningError::Internal)?;
    let to = Coordinate::try_from(to.coordinate).map_err(|_| PlanningError::Internal)?;
    let constraints = context.constraints(departure, RoutingTimeKind::DepartAt);
    match service.street_path(mode, from, to, constraints).await {
        super::super::service::PathAttempt::Found(path) => Ok(Some(path)),
        super::super::service::PathAttempt::Unreachable => Ok(None),
        super::super::service::PathAttempt::Failed(error) => match error.failure_class() {
            ProviderFailureClass::Unreachable => Ok(None),
            ProviderFailureClass::Transient => Err(PlanningError::TemporarilyUnavailable),
            ProviderFailureClass::Internal => Err(PlanningError::Internal),
        },
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
        mode: LegMode::Walking,
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
    service: &crate::domain::transit::TransitPattern,
    route: &crate::domain::transit::TransitRoute,
    boarding: &crate::domain::transit::TransitStop,
    alighting: &crate::domain::transit::TransitStop,
    effective: &EffectiveRide,
    timing_basis: TimingBasis,
) -> Leg {
    let guidance_code = match route.mode {
        TransitMode::Bus => "board_bus",
        TransitMode::Metro => "board_metro",
        TransitMode::Rail => "board_rail",
        TransitMode::HighSpeedRail => "board_high_speed_rail",
    };
    let schedule = matches!(timing_basis, TimingBasis::Schedule);
    Leg {
        id,
        mode: LegMode::Transit,
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
                ("service_label".into(), serde_json::json!(route.label)),
                (
                    "boarding_point_name".into(),
                    serde_json::json!(boarding.name),
                ),
            ]),
        }],
        details: LegDetails::Transit {
            transit_mode: route.mode,
            via_pattern_id: service.id.as_str().into(),
            via_boarding_point_id: boarding.id.as_str().into(),
            via_alighting_point_id: alighting.id.as_str().into(),
            service_label: route.label.clone(),
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
