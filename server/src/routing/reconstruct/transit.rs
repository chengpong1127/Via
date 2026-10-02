use time::OffsetDateTime;

use crate::domain::{
    ids::EffectiveTripId,
    journey::{Guidance, Leg, LegDetails, LegMode, Place, PlanningError},
    ranking::TransferEvidence,
    transit::TransitMode,
    transit_timing::{EffectiveTrip, TimingReliability},
};

use super::{
    assembler::{PendingTransfer, ReconstructedLegs, ReconstructionState},
    evidence, geometry,
};

pub(super) struct PreviousTransit {
    leg_id: String,
    trip_id: EffectiveTripId,
    arrival: OffsetDateTime,
    mode: TransitMode,
}

impl ReconstructionState<'_> {
    pub(super) fn append_transit(
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
        let live_evidence = trip
            .evidence
            .iter()
            .map(evidence::from_realtime_value)
            .collect::<Vec<_>>();
        self.result
            .required_states
            .extend(trip.evidence.iter().map(|status| status.freshness));
        self.result.live_evidence.extend(live_evidence);
        let leg_id = format!("candidate_leg_{}", self.result.legs.len());
        self.result.legs.push(transit_leg(
            leg_id.clone(),
            pattern_data,
            route_data,
            boarding,
            alighting,
            board.departure_at,
            alight.arrival_at,
        ));
        if let Some(pending) = self.pending_transfer.take() {
            record_transfer(
                &mut self.result,
                pending,
                route_data.mode,
                board.departure_at,
            );
        } else if let Some(previous) = &self.previous_transit
            && previous.trip_id != trip.id
        {
            record_transfer(
                &mut self.result,
                PendingTransfer {
                    from_leg_id: previous.leg_id.clone(),
                    from_arrival: previous.arrival,
                    duration_seconds: self.service.transit_runtime.same_stop_transfer_seconds(),
                    from_mode: Some(previous.mode),
                },
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
}

pub(super) fn register_pending_transfer(
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
    let from_mode = match &from_leg.details {
        LegDetails::Transit { transit_mode, .. } => Some(*transit_mode),
        LegDetails::Walking | LegDetails::BikeShare { .. } => None,
    };
    let from_leg_id = from_leg.id.clone();
    let connector_duration = connector_legs.map(|leg| leg.duration_seconds).sum();
    if let Some(pending) = pending_transfer
        .as_mut()
        .filter(|pending| pending.from_leg_id == from_leg_id)
    {
        pending.duration_seconds = pending.duration_seconds.saturating_add(connector_duration);
    } else {
        *pending_transfer = Some(PendingTransfer {
            from_leg_id,
            from_arrival: from_leg.arrival_time,
            duration_seconds: connector_duration,
            from_mode,
        });
    }
}

fn record_transfer(
    result: &mut ReconstructedLegs,
    pending: PendingTransfer,
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
}

fn transit_leg(
    id: String,
    service: &crate::domain::transit::TransitPattern,
    route: &crate::domain::transit::TransitRoute,
    boarding: &crate::domain::transit::TransitStop,
    alighting: &crate::domain::transit::TransitStop,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
) -> Leg {
    let guidance_code = match route.mode {
        TransitMode::Bus => "board_bus",
        TransitMode::Metro => "board_metro",
        TransitMode::Rail => "board_rail",
        TransitMode::HighSpeedRail => "board_high_speed_rail",
    };
    Leg {
        id,
        mode: LegMode::Transit,
        from: transit_place(boarding),
        to: transit_place(alighting),
        departure_time: departure,
        arrival_time: arrival,
        duration_seconds: seconds_between(departure, arrival).unwrap_or(0),
        geometry: geometry::straight_geometry(boarding.coordinate, alighting.coordinate),
        guidance: vec![Guidance {
            code: guidance_code,
            parameters: std::collections::BTreeMap::from([
                ("service_label".into(), route.label.clone()),
                ("boarding_point_name".into(), boarding.name.clone()),
            ]),
        }],
        details: LegDetails::Transit {
            transit_mode: route.mode,
            via_pattern_id: service.id.as_str().into(),
            via_boarding_point_id: boarding.id.as_str().into(),
            via_alighting_point_id: alighting.id.as_str().into(),
            service_label: route.label.clone(),
            headsign: service.headsign.clone(),
        },
        attribution: Vec::new(),
    }
}

pub(super) fn transit_place(stop: &crate::domain::transit::TransitStop) -> Place {
    Place {
        display_name: stop.name.clone(),
        coordinate: stop.coordinate,
        via_id: Some(stop.id.as_str().into()),
    }
}

pub(super) fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> Option<u32> {
    u32::try_from((to - from).whole_seconds()).ok()
}
