use time::OffsetDateTime;

use crate::domain::{
    ids::EffectiveTripId,
    journey::{Guidance, Leg, LegDetails, LegMode, Place, PlanningError},
    ranking::TransferEvidence,
    transit::TransitMode,
    transit_timing::{EffectiveTrip, TimingReliability},
};

use super::{
    assembler::{JourneyAssembly, PendingTransfer, ReconstructionState},
    evidence, geometry,
};

pub(super) struct PreviousTransit {
    trip_id: EffectiveTripId,
    arrival: OffsetDateTime,
    mode: TransitMode,
}

impl ReconstructionState<'_, '_> {
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
            || self.assembly.arrival_time > board.departure_at
            || self.pending_transfer.is_none()
                && self
                    .previous_transit
                    .as_ref()
                    .is_some_and(|previous| previous.trip_id == trip.id)
        {
            return Ok(false);
        }
        let pattern_data = self
            .reconstructor
            .transit_runtime
            .required_pattern(&trip.pattern_id);
        let route_data = self.reconstructor.transit_runtime.route_of(pattern_data);
        let ride_stops = trip
            .stop_times
            .get(board_index..=alight_index)
            .ok_or(PlanningError::Internal)?
            .iter()
            .map(|stop_time| {
                self.reconstructor
                    .transit_runtime
                    .stop(&stop_time.stop_id)
                    .ok_or(PlanningError::Internal)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let live_evidence = trip
            .evidence
            .iter()
            .map(evidence::from_realtime_value)
            .collect::<Vec<_>>();
        self.assembly
            .required_states
            .extend(trip.evidence.iter().map(|status| status.freshness));
        self.assembly.live_evidence.extend(live_evidence);
        let leg_id = format!("candidate_leg_{}", self.assembly.legs.len());
        self.assembly.legs.push(transit_leg(
            leg_id.clone(),
            pattern_data,
            route_data,
            &ride_stops,
            board.departure_at,
            alight.arrival_at,
        )?);
        if let Some(pending) = self.pending_transfer.take() {
            record_transfer(
                &mut self.assembly,
                pending,
                route_data.mode,
                board.departure_at,
            )?;
        } else if let Some(previous) = &self.previous_transit
            && previous.trip_id != trip.id
        {
            record_transfer(
                &mut self.assembly,
                PendingTransfer {
                    from_arrival: previous.arrival,
                    duration_seconds: self
                        .reconstructor
                        .transit_runtime
                        .same_stop_transfer_seconds(),
                    from_mode: Some(previous.mode),
                },
                route_data.mode,
                board.departure_at,
            )?;
        }
        self.previous_transit = Some(PreviousTransit {
            trip_id: trip.id.clone(),
            arrival: alight.arrival_at,
            mode: route_data.mode,
        });
        self.assembly.arrival_time = alight.arrival_at;
        self.assembly.timing_bases.push(trip.timing_basis);
        if trip.timing_reliability == TimingReliability::Fallback {
            self.assembly.timing_reliability = TimingReliability::Fallback;
        }
        evidence::push_unique_mode(&mut self.assembly.travel_modes, LegMode::Transit);
        self.assembly
            .main_pattern_sequence
            .push(pattern_data.id.as_str().to_owned());
        self.current_stop = Some(alight.stop_id.clone());

        Ok(true)
    }
}

pub(super) fn register_pending_transfer(
    assembly: &JourneyAssembly,
    pending_transfer: &mut Option<PendingTransfer>,
) {
    let Some(from_index) = assembly
        .legs
        .iter()
        .rposition(|leg| leg.mode == LegMode::Transit)
    else {
        return;
    };
    let from_leg = &assembly.legs[from_index];
    let from_mode = match &from_leg.details {
        LegDetails::Transit { transit_mode, .. } => Some(*transit_mode),
        LegDetails::Walking | LegDetails::BikeShare { .. } => None,
    };
    *pending_transfer = Some(PendingTransfer {
        from_arrival: from_leg.arrival_time,
        duration_seconds: assembly.legs[from_index + 1..]
            .iter()
            .map(|leg| leg.duration_seconds)
            .sum(),
        from_mode,
    });
}

fn record_transfer(
    assembly: &mut JourneyAssembly,
    pending: PendingTransfer,
    to_mode: TransitMode,
    departure: OffsetDateTime,
) -> Result<(), PlanningError> {
    let available_seconds =
        seconds_between(pending.from_arrival, departure).ok_or(PlanningError::Internal)?;
    let kind = evidence::transfer_kind(pending.from_mode, Some(to_mode));
    assembly.transfer_evidence.push(TransferEvidence {
        kind,
        physical_seconds: pending.duration_seconds,
        available_seconds,
    });
    Ok(())
}

fn transit_leg(
    id: String,
    service: &crate::domain::transit::TransitPattern,
    route: &crate::domain::transit::TransitRoute,
    stops: &[&crate::domain::transit::TransitStop],
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
) -> Result<Leg, PlanningError> {
    let [boarding, intermediate @ .., alighting] = stops else {
        return Err(PlanningError::Internal);
    };
    let intermediate_stops = intermediate
        .iter()
        .map(|stop| transit_place(stop))
        .collect::<Vec<_>>();
    let guidance_code = match route.mode {
        TransitMode::Bus => "board_bus",
        TransitMode::Metro => "board_metro",
        TransitMode::Rail => "board_rail",
        TransitMode::HighSpeedRail => "board_high_speed_rail",
    };
    Ok(Leg {
        id,
        mode: LegMode::Transit,
        from: transit_place(boarding),
        to: transit_place(alighting),
        departure_time: departure,
        arrival_time: arrival,
        duration_seconds: seconds_between(departure, arrival).ok_or(PlanningError::Internal)?,
        geometry: geometry::stop_geometry(stops.iter().map(|stop| stop.coordinate)),
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
            intermediate_stops,
        },
        attribution: Vec::new(),
    })
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
