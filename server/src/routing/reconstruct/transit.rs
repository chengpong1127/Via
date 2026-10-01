use time::OffsetDateTime;

use crate::domain::{
    ids::EffectiveTripId,
    journey::{
        Guidance, JourneyClass, Leg, LegDetails, LegMode, LiveStatusEvidence, Place, PlanningError,
        RankingReason, Transfer,
    },
    ranking::TransferEvidence,
    transit::TransitMode,
    transit_timing::{EffectiveTrip, TimingBasis, TimingReliability},
};

use super::{
    assembler::{PendingTransfer, ReconstructedLegs, ReconstructionState},
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

pub(super) fn transit_place(stop: &crate::domain::transit::TransitStop) -> Place {
    Place {
        display_name: stop.name.clone(),
        coordinate: stop.coordinate,
        kind: "boarding_point",
        via_id: Some(stop.id.as_str().into()),
    }
}

pub(super) fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> Option<u32> {
    u32::try_from((to - from).whole_seconds()).ok()
}
