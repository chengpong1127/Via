use time::OffsetDateTime;

use crate::{
    domain::{
        ids::BoardingPointId,
        journey::{Leg, LegMode, LiveStatusEvidence, PlanningError},
        ranking::TransferEvidence,
        realtime::FreshnessState,
        transit::TransitMode,
        transit_timing::{TimingBasis, TimingReliability},
    },
    routing::{
        JourneyMovement, JourneyMovementKind, RealizedJourney, model::JourneyPlanningContext,
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
    pub(super) bike_reuse_available_at: Option<OffsetDateTime>,
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

pub(super) struct ReconstructionState<'a, 'request> {
    pub(super) reconstructor: &'a JourneyReconstructor,
    pub(super) journey: &'a RealizedJourney,
    pub(super) context: &'a JourneyPlanningContext<'request>,
    pub(super) assembly: JourneyAssembly,
    pub(super) pending_transfer: Option<PendingTransfer>,
    pub(super) previous_transit: Option<PreviousTransit>,
    pub(super) current_stop: Option<BoardingPointId>,
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
                bike_reuse_available_at: None,
                timing_bases: Vec::new(),
                timing_reliability: TimingReliability::Reliable,
            },
            pending_transfer: None,
            previous_transit: None,
            current_stop: None,
        }
    }

    async fn assemble(mut self) -> Result<Option<JourneyAssembly>, PlanningError> {
        let movements = self.journey.movements();
        for movement in movements {
            self.context.ensure_before_deadline()?;
            if !self.append_movement(movement).await? {
                return Ok(None);
            }
        }
        Ok((!self.assembly.legs.is_empty()).then_some(self.assembly))
    }

    async fn append_movement(&mut self, movement: &JourneyMovement) -> Result<bool, PlanningError> {
        match &movement.kind {
            JourneyMovementKind::Walking { .. } => self.append_walk(movement).await,
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
