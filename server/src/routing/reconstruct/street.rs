use time::{Duration, OffsetDateTime};

use crate::{
    api::PlanningRequest,
    domain::{
        ids::BoardingPointId,
        journey::{Geometry, Leg, LegDetails, LegMode, Place, PlanningError},
    },
    routing::{
        model::JourneyPlanningContext,
        realized::{JourneyAnchorKind, JourneyMovement, RealizedJourney},
    },
    street::{
        Coordinate, ProviderFailureClass, RouteQuery, RoutingTimeKind, StreetPath, StreetTravelMode,
    },
};

use super::{
    JourneyReconstructor,
    assembler::{JourneyAssembly, ReconstructionState},
    evidence, geometry,
    transit::register_pending_transfer,
};

impl ReconstructionState<'_, '_> {
    pub(super) fn append_realized_walk(
        &mut self,
        movement: &JourneyMovement,
    ) -> Result<bool, PlanningError> {
        let Some(path) = movement.street_path.as_deref() else {
            return Err(PlanningError::Internal);
        };
        let from = anchor_place(
            self.reconstructor,
            self.context.request,
            self.journey,
            movement.from_anchor,
        )
        .ok_or(PlanningError::Internal)?;
        let to = anchor_place(
            self.reconstructor,
            self.context.request,
            self.journey,
            movement.to_anchor,
        )
        .ok_or(PlanningError::Internal)?;
        let connector_start = self.assembly.legs.len();
        append_routed_walking(&from, &to, path.clone(), &mut self.assembly);
        if matches!(
            movement.kind,
            crate::routing::JourneyMovementKind::Walking {
                kind: crate::routing::WalkingMovementKind::Transfer,
                ..
            }
        ) {
            register_pending_transfer(&self.assembly, &mut self.pending_transfer, connector_start);
        }
        self.current_stop = anchor_boarding_point(self.journey, movement.to_anchor);
        Ok(true)
    }

    pub(super) async fn append_access(
        &mut self,
        movement: &JourneyMovement,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Result<bool, PlanningError> {
        if self.current_anchor != 0 {
            return Ok(false);
        }
        let first_stop = self
            .reconstructor
            .stop_place(
                &anchor_boarding_point(self.journey, movement.to_anchor)
                    .ok_or(PlanningError::Internal)?,
            )
            .ok_or(PlanningError::Internal)?;
        append_hydrated_walking(
            self.reconstructor,
            self.context,
            &self.origin,
            &first_stop,
            duration_seconds,
            distance_metres,
            &mut self.assembly,
        )
        .await?;
        self.current_stop = anchor_boarding_point(self.journey, movement.to_anchor);

        Ok(true)
    }

    pub(super) fn append_surface_transfer(
        &mut self,
        movement: &JourneyMovement,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Result<bool, PlanningError> {
        let from_stop = self
            .current_stop
            .clone()
            .or_else(|| anchor_boarding_point(self.journey, movement.from_anchor))
            .ok_or(PlanningError::Internal)?;
        let from = self
            .reconstructor
            .stop_place(&from_stop)
            .ok_or(PlanningError::Internal)?;
        let to_stop = anchor_boarding_point(self.journey, movement.to_anchor)
            .ok_or(PlanningError::Internal)?;
        if Some(to_stop.clone()) == self.current_stop {
            return Ok(false);
        }
        let to = self
            .reconstructor
            .stop_place(&to_stop)
            .ok_or(PlanningError::Internal)?;
        let connector_start = self.assembly.legs.len();
        append_static_walking(
            &from,
            &to,
            duration_seconds,
            distance_metres,
            &mut self.assembly,
        );
        register_pending_transfer(&self.assembly, &mut self.pending_transfer, connector_start);
        self.current_stop = Some(to_stop);

        Ok(true)
    }

    pub(super) async fn append_egress(
        &mut self,
        movement: &JourneyMovement,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Result<bool, PlanningError> {
        if movement.to_anchor != self.final_anchor {
            return Ok(false);
        }
        let final_stop_id = self
            .current_stop
            .clone()
            .or_else(|| anchor_boarding_point(self.journey, movement.from_anchor))
            .ok_or(PlanningError::Internal)?;
        let final_stop = self
            .reconstructor
            .stop_place(&final_stop_id)
            .ok_or(PlanningError::Internal)?;
        append_hydrated_walking(
            self.reconstructor,
            self.context,
            &final_stop,
            &self.destination,
            duration_seconds,
            distance_metres,
            &mut self.assembly,
        )
        .await?;

        Ok(true)
    }
}

pub(super) fn anchor_boarding_point(
    journey: &RealizedJourney,
    anchor: usize,
) -> Option<BoardingPointId> {
    match &journey.anchors().get(anchor)?.kind {
        JourneyAnchorKind::BoardingPoint { id } => Some(id.clone()),
        JourneyAnchorKind::Origin | JourneyAnchorKind::Destination => None,
    }
}

pub(super) fn anchor_place(
    reconstructor: &JourneyReconstructor,
    request: &PlanningRequest,
    journey: &RealizedJourney,
    anchor: usize,
) -> Option<Place> {
    match &journey.anchors().get(anchor)?.kind {
        JourneyAnchorKind::Origin => Some(reconstructor.origin_place(request)),
        JourneyAnchorKind::BoardingPoint { id } => reconstructor.stop_place(id),
        JourneyAnchorKind::Destination => Some(reconstructor.destination_place(request)),
    }
}

pub(super) fn append_routed_walking(
    from: &Place,
    to: &Place,
    path: StreetPath,
    assembly: &mut JourneyAssembly,
) {
    let departure = assembly.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", assembly.legs.len());
    assembly
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    assembly.walking_seconds = assembly.walking_seconds.saturating_add(path.duration.0);
    assembly.arrival_time = arrival;
    evidence::push_unique_mode(&mut assembly.travel_modes, LegMode::Walking);
}

pub(super) async fn append_hydrated_walking(
    reconstructor: &JourneyReconstructor,
    context: &JourneyPlanningContext<'_>,
    from: &Place,
    to: &Place,
    fallback_seconds: u32,
    fallback_metres: u32,
    assembly: &mut JourneyAssembly,
) -> Result<(), PlanningError> {
    let path = hydrated_or_synthetic(
        reconstructor,
        context,
        from,
        to,
        StreetTravelMode::Walking,
        assembly.arrival_time,
        (fallback_seconds, fallback_metres),
    )
    .await?;
    let departure = assembly.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", assembly.legs.len());
    assembly
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    assembly.walking_seconds = assembly.walking_seconds.saturating_add(path.duration.0);
    assembly.arrival_time = arrival;
    evidence::push_unique_mode(&mut assembly.travel_modes, LegMode::Walking);
    Ok(())
}

fn append_static_walking(
    from: &Place,
    to: &Place,
    duration_seconds: u32,
    distance_metres: u32,
    assembly: &mut JourneyAssembly,
) {
    let path = synthetic_street_path(from, to, duration_seconds, distance_metres);
    let departure = assembly.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(duration_seconds));
    let leg_id = format!("candidate_leg_{}", assembly.legs.len());
    assembly
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    assembly.walking_seconds = assembly.walking_seconds.saturating_add(duration_seconds);
    assembly.arrival_time = arrival;
    evidence::push_unique_mode(&mut assembly.travel_modes, LegMode::Walking);
}

async fn hydrated_or_synthetic(
    reconstructor: &JourneyReconstructor,
    context: &JourneyPlanningContext<'_>,
    from: &Place,
    to: &Place,
    mode: StreetTravelMode,
    departure: OffsetDateTime,
    fallback: (u32, u32),
) -> Result<StreetPath, PlanningError> {
    match hydrate_path(reconstructor, context, from, to, mode, departure).await? {
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
    reconstructor: &JourneyReconstructor,
    context: &JourneyPlanningContext<'_>,
    from: &Place,
    to: &Place,
    mode: StreetTravelMode,
    departure: OffsetDateTime,
) -> Result<Option<StreetPath>, PlanningError> {
    let from = Coordinate::try_from(from.coordinate).map_err(|_| PlanningError::Internal)?;
    let to = Coordinate::try_from(to.coordinate).map_err(|_| PlanningError::Internal)?;
    let constraints = context.constraints(departure, RoutingTimeKind::DepartAt);
    match reconstructor
        .street_routing
        .route(RouteQuery {
            travel_mode: mode,
            origin: from,
            destination: to,
            constraints,
        })
        .await
    {
        Ok(path) => Ok(Some(path)),
        Err(error) => match error.failure_class() {
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
        duration_seconds: path.duration.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: geometry::encode_polyline6(&path.geometry),
        },
        guidance: geometry::guidance(&path.maneuvers),
        details: LegDetails::Walking,
        attribution: path.attribution.clone(),
    }
}
