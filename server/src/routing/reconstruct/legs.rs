use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        ids::{BoardingPointId, EffectiveTripId},
        journey::{
            Geometry, Guidance, JourneyClass, Leg, LegDetails, LegMode, LiveStatusEvidence, Place,
            PlanningError, RankingReason, Transfer,
        },
        ranking::TransferEvidence,
        realtime::FreshnessState,
        transit::TransitMode,
        transit_timing::{EffectiveTrip, TimingBasis, TimingReliability},
    },
    street::{Coordinate, ProviderFailureClass, RoutingTimeKind, StreetPath, StreetTravelMode},
};

use super::{
    super::{
        service::{PlanningContext, RoutingService},
        variant::{
            JourneyAnchorKind, JourneyMovement, JourneyMovementKind, JourneyPathMovement,
            JourneyPathStep, JourneyVariant, WalkingMovementKind,
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
    ReconstructionState::new(service, request, variant, context)
        .assemble()
        .await
}

enum TerminalEgressOutcome {
    NotApplicable,
    Appended,
    Invalid,
}

struct ReconstructionState<'a> {
    service: &'a RoutingService,
    request: &'a PlanningRequest,
    variant: &'a JourneyVariant,
    context: &'a PlanningContext,
    origin: Place,
    destination: Place,
    result: ReconstructedLegs,
    pending_transfer: Option<PendingTransfer>,
    previous_transit: Option<PreviousTransit>,
    current_stop: Option<BoardingPointId>,
    current_anchor: usize,
    final_anchor: usize,
    skip_next_step: bool,
}

impl<'a> ReconstructionState<'a> {
    fn new(
        service: &'a RoutingService,
        request: &'a PlanningRequest,
        variant: &'a JourneyVariant,
        context: &'a PlanningContext,
    ) -> Self {
        Self {
            service,
            request,
            variant,
            context,
            origin: service.origin_place(request),
            destination: service.destination_place(request),
            result: ReconstructedLegs {
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
            },
            pending_transfer: None,
            previous_transit: None,
            current_stop: None,
            current_anchor: 0,
            final_anchor: variant.skeleton().anchors().len().saturating_sub(1),
            skip_next_step: false,
        }
    }

    async fn assemble(mut self) -> Result<Option<ReconstructedLegs>, PlanningError> {
        let steps = self.variant.path();
        for (step_index, step) in steps.iter().enumerate() {
            self.context.ensure_before_deadline()?;
            if self.skip_next_step {
                self.skip_next_step = false;
                continue;
            }
            if step.from_anchor != self.current_anchor || step.to_anchor <= step.from_anchor {
                return Ok(None);
            }
            match self.append_terminal_egress(steps, step_index).await? {
                TerminalEgressOutcome::NotApplicable => {}
                TerminalEgressOutcome::Appended => continue,
                TerminalEgressOutcome::Invalid => return Ok(None),
            }
            if !self.append_step(step).await? {
                return Ok(None);
            }
            self.current_anchor = step.to_anchor;
        }
        if self.current_anchor != self.final_anchor {
            return Ok(None);
        }
        Ok((!self.result.legs.is_empty()).then_some(self.result))
    }

    async fn append_terminal_egress(
        &mut self,
        steps: &[JourneyPathStep],
        step_index: usize,
    ) -> Result<TerminalEgressOutcome, PlanningError> {
        let Some((from_stop, egress_stop, duration_seconds)) =
            terminal_surface_transfer_egress(self.variant, steps, step_index, self.final_anchor)
        else {
            return Ok(TerminalEgressOutcome::NotApplicable);
        };
        if self.current_stop.as_ref() != Some(&from_stop) {
            return Ok(TerminalEgressOutcome::Invalid);
        }
        let from = self
            .service
            .stop_place(&from_stop)
            .ok_or(PlanningError::Internal)?;
        append_hydrated_walking(
            self.service,
            self.context,
            &from,
            &self.destination,
            duration_seconds,
            0,
            &mut self.result,
        )
        .await?;
        self.current_stop = Some(egress_stop);
        self.current_anchor = steps[step_index + 1].to_anchor;
        self.skip_next_step = true;
        Ok(TerminalEgressOutcome::Appended)
    }

    async fn append_step(&mut self, step: &JourneyPathStep) -> Result<bool, PlanningError> {
        match &step.movement {
            JourneyPathMovement::Baseline { movement_index } => {
                let variant = self.variant;
                let movement = variant
                    .skeleton()
                    .movements()
                    .get(*movement_index)
                    .ok_or(PlanningError::Internal)?;
                if movement.from_anchor != step.from_anchor || movement.to_anchor != step.to_anchor
                {
                    return Ok(false);
                }
                match &movement.kind {
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Access,
                        duration_seconds,
                        distance_metres,
                    } => {
                        self.append_access(step, *duration_seconds, *distance_metres)
                            .await
                    }
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Transfer,
                        duration_seconds,
                        distance_metres,
                    } => self.append_surface_transfer(step, *duration_seconds, *distance_metres),
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Egress,
                        duration_seconds,
                        distance_metres,
                    } => {
                        self.append_egress(step, *duration_seconds, *distance_metres)
                            .await
                    }
                    JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Direct,
                        ..
                    } => self.append_direct_walk(step, movement),
                    JourneyMovementKind::Transit {
                        trip,
                        board_index,
                        alight_index,
                    } => self.append_transit(trip, *board_index, *alight_index),
                }
            }
            JourneyPathMovement::BikeShare { shortcut } => self.append_bike(step, shortcut).await,
        }
    }

    async fn append_access(
        &mut self,
        step: &JourneyPathStep,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Result<bool, PlanningError> {
        if self.current_anchor != 0 {
            return Ok(false);
        }
        let first_stop = self
            .service
            .stop_place(
                &anchor_boarding_point(self.variant, step.to_anchor)
                    .ok_or(PlanningError::Internal)?,
            )
            .ok_or(PlanningError::Internal)?;
        append_hydrated_walking(
            self.service,
            self.context,
            &self.origin,
            &first_stop,
            duration_seconds,
            distance_metres,
            &mut self.result,
        )
        .await?;
        self.current_stop = anchor_boarding_point(self.variant, step.to_anchor);

        Ok(true)
    }

    fn append_surface_transfer(
        &mut self,
        step: &JourneyPathStep,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Result<bool, PlanningError> {
        let from_stop = self
            .current_stop
            .clone()
            .or_else(|| anchor_boarding_point(self.variant, step.from_anchor))
            .ok_or(PlanningError::Internal)?;
        let from = self
            .service
            .stop_place(&from_stop)
            .ok_or(PlanningError::Internal)?;
        let to_stop =
            anchor_boarding_point(self.variant, step.to_anchor).ok_or(PlanningError::Internal)?;
        if Some(to_stop.clone()) == self.current_stop {
            return Ok(false);
        }
        let to = self
            .service
            .stop_place(&to_stop)
            .ok_or(PlanningError::Internal)?;
        let connector_start = self.result.legs.len();
        append_static_walking(
            &from,
            &to,
            duration_seconds,
            distance_metres,
            &mut self.result,
        );
        register_pending_transfer(&self.result, &mut self.pending_transfer, connector_start);
        self.current_stop = Some(to_stop);

        Ok(true)
    }

    async fn append_egress(
        &mut self,
        step: &JourneyPathStep,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Result<bool, PlanningError> {
        if step.to_anchor != self.final_anchor {
            return Ok(false);
        }
        let final_stop_id = self
            .current_stop
            .clone()
            .or_else(|| anchor_boarding_point(self.variant, step.from_anchor))
            .ok_or(PlanningError::Internal)?;
        let final_stop = self
            .service
            .stop_place(&final_stop_id)
            .ok_or(PlanningError::Internal)?;
        append_hydrated_walking(
            self.service,
            self.context,
            &final_stop,
            &self.destination,
            duration_seconds,
            distance_metres,
            &mut self.result,
        )
        .await?;

        Ok(true)
    }

    fn append_direct_walk(
        &mut self,
        step: &JourneyPathStep,
        movement: &JourneyMovement,
    ) -> Result<bool, PlanningError> {
        if self.current_anchor != 0 || step.to_anchor != self.final_anchor {
            return Ok(false);
        }
        let Some(path) = movement.street_path.as_deref() else {
            return Err(PlanningError::Internal);
        };
        append_routed_walking(
            &self.origin,
            &self.destination,
            path.clone(),
            &mut self.result,
        );

        Ok(true)
    }

    fn append_transit(
        &mut self,
        trip: &EffectiveTrip,
        board_index: usize,
        alight_index: usize,
    ) -> Result<bool, PlanningError> {
        let board = trip
            .stop_times
            .get(board_index)
            .ok_or(PlanningError::Internal)?;
        let alight = trip
            .stop_times
            .get(alight_index)
            .ok_or(PlanningError::Internal)?;
        if Some(board.stop_id.clone()) != self.current_stop
            || self.result.arrival_time > board.departure_at
        {
            return Ok(false);
        }
        let pattern_data = self
            .service
            .transit_runtime
            .pattern(&trip.pattern_id)
            .ok_or(PlanningError::Internal)?;
        let route_data = self.service.transit_runtime.route_of(pattern_data);
        let boarding = self
            .service
            .transit_runtime
            .stop(&board.stop_id)
            .ok_or(PlanningError::Internal)?;
        let alighting = self
            .service
            .transit_runtime
            .stop(&alight.stop_id)
            .ok_or(PlanningError::Internal)?;
        let live = trip
            .evidence
            .iter()
            .map(evidence::from_realtime_value)
            .collect::<Vec<_>>();
        self.result
            .required_states
            .extend(trip.evidence.iter().map(|status| status.freshness.state));
        self.result.live_evidence.extend(live.clone());
        let leg_id = format!("candidate_leg_{}", self.result.legs.len());
        self.result.legs.push(transit_leg(
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
        if let Some(pending) = self.pending_transfer.take() {
            add_transfer(
                &mut self.result,
                pending,
                leg_id.clone(),
                route_data.mode,
                board.departure_at,
            );
        } else if let Some(previous) = &self.previous_transit
            && previous.trip_id != trip.id
        {
            add_transfer(
                &mut self.result,
                PendingTransfer {
                    from_leg_id: previous.leg_id.clone(),
                    from_arrival: previous.arrival,
                    duration_seconds: self.service.transit_runtime.same_stop_transfer_seconds(),
                    connector_leg_ids: Vec::new(),
                    from_mode: Some(previous.mode),
                },
                leg_id.clone(),
                route_data.mode,
                board.departure_at,
            );
        }
        self.previous_transit = Some(PreviousTransit {
            leg_id,
            trip_id: trip.id.clone(),
            arrival: alight.arrival_at,
            mode: route_data.mode,
        });
        self.result.arrival_time = alight.arrival_at;
        self.result.timing_bases.push(trip.timing_basis);
        if trip.timing_reliability == TimingReliability::Fallback {
            self.result.timing_reliability = TimingReliability::Fallback;
        }
        evidence::push_unique_mode(&mut self.result.travel_modes, LegMode::Transit);
        self.result
            .main_pattern_sequence
            .push(pattern_data.id.as_str().to_owned());
        self.current_stop = Some(alight.stop_id.clone());

        Ok(true)
    }

    async fn append_bike(
        &mut self,
        step: &JourneyPathStep,
        shortcut: &crate::routing::BikeShortcut,
    ) -> Result<bool, PlanningError> {
        if shortcut.from_anchor != step.from_anchor || shortcut.to_anchor != step.to_anchor {
            return Ok(false);
        }
        if matches!(
            self.variant.skeleton().anchors()[step.from_anchor].kind,
            JourneyAnchorKind::Destination
        ) || matches!(
            self.variant.skeleton().anchors()[step.to_anchor].kind,
            JourneyAnchorKind::Origin
        ) {
            return Ok(false);
        }
        let source_stop = anchor_boarding_point(self.variant, step.from_anchor);
        if source_stop
            .as_ref()
            .is_some_and(|source_stop| self.current_stop.as_ref() != Some(source_stop))
        {
            return Ok(false);
        }
        let from = anchor_place(self.service, self.request, self.variant, step.from_anchor)
            .ok_or(PlanningError::Internal)?;
        let to = anchor_place(self.service, self.request, self.variant, step.to_anchor)
            .ok_or(PlanningError::Internal)?;
        let connector_start = self.result.legs.len();
        if !append_bike_share(
            self.service,
            self.context,
            &from,
            &to,
            shortcut,
            &mut self.result,
        )
        .await?
        {
            return Ok(false);
        }
        register_pending_transfer(&self.result, &mut self.pending_transfer, connector_start);
        if let Some(to_stop) = anchor_boarding_point(self.variant, step.to_anchor) {
            self.current_stop = Some(to_stop);
        }

        Ok(true)
    }
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

fn anchor_boarding_point(variant: &JourneyVariant, anchor: usize) -> Option<BoardingPointId> {
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
