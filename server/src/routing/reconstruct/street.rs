use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        ids::BoardingPointId,
        journey::{Geometry, Leg, LegDetails, LegMode, Place, PlanningError},
    },
    routing::{
        service::{PlanningContext, RoutingService},
        variant::{JourneyAnchorKind, JourneyMovement, JourneyPathStep, JourneyVariant},
    },
    street::{Coordinate, ProviderFailureClass, RoutingTimeKind, StreetPath, StreetTravelMode},
};

use super::{
    assembler::{ReconstructedLegs, ReconstructionState},
    evidence, geometry,
    transit::register_pending_transfer,
};

impl ReconstructionState<'_> {
    pub(super) async fn append_access(
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

    pub(super) fn append_surface_transfer(
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

    pub(super) async fn append_egress(
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

    pub(super) fn append_direct_walk(
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
}

pub(super) fn anchor_boarding_point(
    variant: &JourneyVariant,
    anchor: usize,
) -> Option<BoardingPointId> {
    match &variant.skeleton().anchors().get(anchor)?.kind {
        JourneyAnchorKind::BoardingPoint { id } => Some(id.clone()),
        JourneyAnchorKind::Origin | JourneyAnchorKind::Destination => None,
    }
}

pub(super) fn anchor_place(
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

pub(super) fn append_routed_walking(
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

pub(super) async fn append_hydrated_walking(
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

pub(super) async fn hydrate_path(
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
