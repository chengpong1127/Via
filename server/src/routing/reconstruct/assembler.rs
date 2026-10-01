use time::OffsetDateTime;

use crate::{
    api::PlanningRequest,
    domain::{
        ids::BoardingPointId,
        journey::{Leg, LegMode, LiveStatusEvidence, Place, PlanningError, Transfer},
        ranking::TransferEvidence,
        realtime::FreshnessState,
        transit::TransitMode,
        transit_timing::{TimingBasis, TimingReliability},
    },
    routing::{
        JourneyMovementKind, JourneyPathMovement, JourneyPathStep, JourneyVariant,
        WalkingMovementKind,
        service::{PlanningContext, RoutingService},
    },
};

use super::transit::PreviousTransit;

pub(super) struct ReconstructedLegs {
    pub(super) legs: Vec<Leg>,
    pub(super) transfers: Vec<Transfer>,
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
    pub(super) connector_leg_ids: Vec<String>,
    pub(super) from_mode: Option<TransitMode>,
}

pub(super) async fn assemble(
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

pub(super) struct ReconstructionState<'a> {
    pub(super) service: &'a RoutingService,
    pub(super) request: &'a PlanningRequest,
    pub(super) variant: &'a JourneyVariant,
    pub(super) context: &'a PlanningContext,
    pub(super) origin: Place,
    pub(super) destination: Place,
    pub(super) result: ReconstructedLegs,
    pub(super) pending_transfer: Option<PendingTransfer>,
    pub(super) previous_transit: Option<PreviousTransit>,
    pub(super) current_stop: Option<BoardingPointId>,
    pub(super) current_anchor: usize,
    pub(super) final_anchor: usize,
    pub(super) skip_next_step: bool,
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
        super::street::append_hydrated_walking(
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
        super::street::anchor_boarding_point(variant, transfer_step.from_anchor)?,
        super::street::anchor_boarding_point(variant, transfer_step.to_anchor)?,
        (*transfer_seconds).saturating_add(*egress_seconds),
    ))
}
