use time::OffsetDateTime;

use crate::{
    domain::{
        ids::BoardingPointId,
        journey::{Leg, LegMode, LiveStatusEvidence, Place, PlanningError},
        ranking::TransferEvidence,
        realtime::FreshnessState,
        transit::TransitMode,
        transit_timing::{TimingBasis, TimingReliability},
    },
    routing::{
        JourneyMovement, JourneyMovementKind, RealizedJourney, WalkingMovementKind,
        model::JourneyPlanningContext,
    },
};

use super::{JourneyReconstructor, transit::PreviousTransit};

pub(super) struct JourneyAssembly {
    pub(super) legs: Vec<Leg>,
    pub(super) arrival_time: OffsetDateTime,
    pub(super) live_evidence: Vec<LiveStatusEvidence>,
    pub(super) required_states: Vec<FreshnessState>,
    pub(super) travel_modes: Vec<LegMode>,
    pub(super) main_pattern_sequence: Vec<String>,
    pub(super) transfer_evidence: Vec<TransferEvidence>,
    pub(super) walking_seconds: u32,
    pub(super) bicycle_seconds: u32,
    pub(super) bike_share_uses: u32,
    pub(super) timing_bases: Vec<TimingBasis>,
    pub(super) timing_reliability: TimingReliability,
}

pub(super) struct PendingTransfer {
    pub(super) from_leg_id: String,
    pub(super) from_arrival: OffsetDateTime,
    pub(super) duration_seconds: u32,
    pub(super) from_mode: Option<TransitMode>,
}

pub(super) async fn assemble(
    reconstructor: &JourneyReconstructor,
    journey: &RealizedJourney,
    context: &JourneyPlanningContext<'_>,
) -> Result<Option<JourneyAssembly>, PlanningError> {
    ReconstructionState::new(reconstructor, journey, context)
        .assemble()
        .await
}

enum TerminalEgressOutcome {
    NotApplicable,
    Appended,
    Invalid,
}

pub(super) struct ReconstructionState<'a, 'request> {
    pub(super) reconstructor: &'a JourneyReconstructor,
    pub(super) journey: &'a RealizedJourney,
    pub(super) context: &'a JourneyPlanningContext<'request>,
    pub(super) origin: Place,
    pub(super) destination: Place,
    pub(super) assembly: JourneyAssembly,
    pub(super) pending_transfer: Option<PendingTransfer>,
    pub(super) previous_transit: Option<PreviousTransit>,
    pub(super) current_stop: Option<BoardingPointId>,
    pub(super) current_anchor: usize,
    pub(super) final_anchor: usize,
    pub(super) skip_next_movement: bool,
}

impl<'a, 'request> ReconstructionState<'a, 'request> {
    fn new(
        reconstructor: &'a JourneyReconstructor,
        journey: &'a RealizedJourney,
        context: &'a JourneyPlanningContext<'request>,
    ) -> Self {
        Self {
            reconstructor,
            journey,
            context,
            origin: reconstructor.origin_place(context.request),
            destination: reconstructor.destination_place(context.request),
            assembly: JourneyAssembly {
                legs: Vec::new(),
                arrival_time: journey.departure_at(),
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
            final_anchor: journey.anchors().len().saturating_sub(1),
            skip_next_movement: false,
        }
    }

    async fn assemble(mut self) -> Result<Option<JourneyAssembly>, PlanningError> {
        let movements = self.journey.movements();
        for (movement_index, movement) in movements.iter().enumerate() {
            self.context.ensure_before_deadline()?;
            if self.skip_next_movement {
                self.skip_next_movement = false;
                continue;
            }
            if movement.from_anchor != self.current_anchor
                || movement.to_anchor <= movement.from_anchor
            {
                return Ok(None);
            }
            match self
                .append_terminal_egress(movements, movement_index)
                .await?
            {
                TerminalEgressOutcome::NotApplicable => {}
                TerminalEgressOutcome::Appended => continue,
                TerminalEgressOutcome::Invalid => return Ok(None),
            }
            if !self.append_movement(movement).await? {
                return Ok(None);
            }
            self.current_anchor = movement.to_anchor;
        }
        if self.current_anchor != self.final_anchor {
            return Ok(None);
        }
        Ok((!self.assembly.legs.is_empty()).then_some(self.assembly))
    }

    async fn append_terminal_egress(
        &mut self,
        movements: &[JourneyMovement],
        movement_index: usize,
    ) -> Result<TerminalEgressOutcome, PlanningError> {
        let Some((from_stop, egress_stop, duration_seconds)) = terminal_surface_transfer_egress(
            self.journey,
            movements,
            movement_index,
            self.final_anchor,
        )?
        else {
            return Ok(TerminalEgressOutcome::NotApplicable);
        };
        if self.current_stop.as_ref() != Some(&from_stop) {
            return Ok(TerminalEgressOutcome::Invalid);
        }
        let from = self.reconstructor.stop_place(&from_stop)?;
        super::street::append_hydrated_walking(
            self.reconstructor,
            self.context,
            &from,
            &self.destination,
            duration_seconds,
            0,
            &mut self.assembly,
        )
        .await?;
        self.current_stop = Some(egress_stop);
        self.current_anchor = movements[movement_index + 1].to_anchor;
        self.skip_next_movement = true;
        Ok(TerminalEgressOutcome::Appended)
    }

    async fn append_movement(&mut self, movement: &JourneyMovement) -> Result<bool, PlanningError> {
        if movement.street_path.is_some() {
            return self.append_realized_walk(movement);
        }
        match &movement.kind {
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Access,
                duration_seconds,
                distance_metres,
            } => {
                self.append_access(movement, *duration_seconds, *distance_metres)
                    .await
            }
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Transfer,
                duration_seconds,
                distance_metres,
            } => self.append_surface_transfer(movement, *duration_seconds, *distance_metres),
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Egress,
                duration_seconds,
                distance_metres,
            } => {
                self.append_egress(movement, *duration_seconds, *distance_metres)
                    .await
            }
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Direct,
                ..
            } => Ok(false),
            JourneyMovementKind::Transit {
                trip,
                board_index,
                alight_index,
            } => self.append_transit(trip, *board_index, *alight_index),
            JourneyMovementKind::BikeShare {
                movement: bike_movement,
            } => self.append_bike(movement, bike_movement).await,
        }
    }
}

fn terminal_surface_transfer_egress(
    journey: &RealizedJourney,
    movements: &[JourneyMovement],
    index: usize,
    final_anchor: usize,
) -> Result<
    Option<(
        crate::domain::ids::BoardingPointId,
        crate::domain::ids::BoardingPointId,
        u32,
    )>,
    PlanningError,
> {
    let Some(transfer_movement) = movements.get(index) else {
        return Ok(None);
    };
    let Some(egress_movement) = movements.get(index + 1) else {
        return Ok(None);
    };
    if transfer_movement.street_path.is_some() || egress_movement.street_path.is_some() {
        return Ok(None);
    }
    let JourneyMovementKind::Walking {
        kind: WalkingMovementKind::Transfer,
        duration_seconds: transfer_seconds,
        ..
    } = &transfer_movement.kind
    else {
        return Ok(None);
    };
    let JourneyMovementKind::Walking {
        kind: WalkingMovementKind::Egress,
        duration_seconds: egress_seconds,
        ..
    } = &egress_movement.kind
    else {
        return Ok(None);
    };
    if transfer_movement.to_anchor != egress_movement.from_anchor
        || egress_movement.to_anchor != final_anchor
    {
        return Ok(None);
    }
    let from_stop = super::street::anchor_boarding_point(journey, transfer_movement.from_anchor)
        .ok_or(PlanningError::Internal)?;
    let egress_stop = super::street::anchor_boarding_point(journey, transfer_movement.to_anchor)
        .ok_or(PlanningError::Internal)?;
    Ok(Some((
        from_stop,
        egress_stop,
        (*transfer_seconds).saturating_add(*egress_seconds),
    )))
}
