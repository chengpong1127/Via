use time::OffsetDateTime;

use crate::{
    domain::{
        ids::{BoardingPointId, EffectiveTripId},
        journey::{Guidance, Leg, LegDetails, LegMode, Place, PlanningError, StopVisit},
        ranking::{TransferEvidence, TransferKind},
        transit::network::TransitMode,
        transit::timing::{EffectiveStopTime, EffectiveTrip, TimingReliability},
    },
    street::StreetTravelMode,
};

use super::{
    assembler::{JourneyAssembly, PendingTransfer, ReconstructionState},
    evidence, geometry,
    street::{append_routed_walking, hydrate_path},
};

pub(super) struct PreviousTransit {
    trip_id: EffectiveTripId,
    arrival: OffsetDateTime,
    mode: TransitMode,
}

impl ReconstructionState<'_, '_> {
    pub(super) async fn append_transit(
        &mut self,
        trip: &EffectiveTrip,
        board_index: usize,
        alight_index: usize,
        to_anchor: usize,
    ) -> Result<bool, PlanningError> {
        let board = trip
            .stop_times
            .get(board_index)
            .ok_or(PlanningError::Internal)?;
        let alight = trip
            .stop_times
            .get(alight_index)
            .ok_or(PlanningError::Internal)?;
        let Some(current_stop) = self.current_stop.clone() else {
            return Ok(false);
        };
        if !self
            .reconstructor
            .transit
            .dense()
            .same_place(&current_stop, &board.stop_id)
        {
            return Ok(false);
        }
        if !self
            .walk_between_stops(&current_stop, &board.stop_id)
            .await?
        {
            return Ok(false);
        }
        if self.assembly.arrival_time > board.departure_at
            || self.pending_transfer.is_none()
                && self
                    .previous_transit
                    .as_ref()
                    .is_some_and(|previous| previous.trip_id == trip.id)
        {
            return Ok(false);
        }
        let pattern = self
            .reconstructor
            .transit
            .required_pattern(&trip.pattern_id);
        let route = self.reconstructor.transit.required_route(&pattern.route_id);
        let ride_times = trip
            .stop_times
            .get(board_index..=alight_index)
            .ok_or(PlanningError::Internal)?;
        let ride_stops = ride_times
            .iter()
            .map(|stop_time| {
                self.reconstructor
                    .transit
                    .stop(&stop_time.stop_id)
                    .ok_or(PlanningError::Internal)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let live_evidence = trip
            .evidence
            .iter()
            .map(evidence::from_realtime_value)
            .collect::<Vec<_>>();
        self.assembly.live_evidence.extend(live_evidence);
        let leg_id = self.assembly.next_leg_id();
        self.assembly.legs.push(transit_leg(
            leg_id.clone(),
            pattern,
            route,
            &ride_stops,
            ride_times,
            board.departure_at,
            alight.arrival_at,
        )?);
        if let Some(pending) = self.pending_transfer.take() {
            record_transfer(&mut self.assembly, pending, route.mode, board.departure_at)?;
        } else if let Some(previous) = &self.previous_transit
            && previous.trip_id != trip.id
        {
            record_transfer(
                &mut self.assembly,
                PendingTransfer {
                    from_arrival: previous.arrival,
                    duration_seconds: self.reconstructor.transit.same_stop_transfer_seconds(),
                    from_mode: Some(previous.mode),
                },
                route.mode,
                board.departure_at,
            )?;
        }
        self.previous_transit = Some(PreviousTransit {
            trip_id: trip.id.clone(),
            arrival: alight.arrival_at,
            mode: route.mode,
        });
        self.assembly.arrival_time = alight.arrival_at;
        if trip.timing_reliability == TimingReliability::Fallback {
            self.assembly.timing_reliability = TimingReliability::Fallback;
        }
        evidence::push_unique_mode(&mut self.assembly.travel_modes, LegMode::Transit);
        self.assembly
            .main_pattern_sequence
            .push(pattern.id.as_str().to_owned());
        let anchor_stop = self
            .anchor_boarding_point(to_anchor)
            .ok_or(PlanningError::Internal)?;
        if !self
            .walk_between_stops(&alight.stop_id, &anchor_stop)
            .await?
        {
            return Ok(false);
        }
        self.current_stop = Some(anchor_stop);

        Ok(true)
    }

    async fn walk_between_stops(
        &mut self,
        from: &BoardingPointId,
        to: &BoardingPointId,
    ) -> Result<bool, PlanningError> {
        if from == to {
            return Ok(true);
        }
        let from = self.reconstructor.stop_place(from)?;
        let to = self.reconstructor.stop_place(to)?;
        let Some(path) = hydrate_path(
            self.reconstructor,
            self.context,
            &from,
            &to,
            StreetTravelMode::Walking,
        )
        .await?
        else {
            return Ok(false);
        };
        append_routed_walking(&from, &to, path, &mut self.assembly);
        register_pending_transfer(&self.assembly, &mut self.pending_transfer);
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
    assembly.transfer_evidence.push(TransferEvidence {
        kind: TransferKind::between(pending.from_mode, to_mode),
        physical_seconds: pending.duration_seconds,
        available_seconds,
    });
    Ok(())
}

fn transit_leg(
    id: String,
    pattern: &crate::domain::transit::network::TransitPattern,
    route: &crate::domain::transit::network::TransitRoute,
    stops: &[&crate::domain::transit::network::TransitStop],
    stop_times: &[EffectiveStopTime],
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
) -> Result<Leg, PlanningError> {
    let [boarding, intermediate @ .., alighting] = stops else {
        return Err(PlanningError::Internal);
    };
    let intermediate_stops = intermediate
        .iter()
        .zip(stop_times.iter().skip(1))
        .map(|(stop, stop_time)| StopVisit {
            place: transit_place(stop),
            arrival_time: stop_time.arrival_at,
        })
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
            via_pattern_id: pattern.id.as_str().into(),
            via_boarding_point_id: boarding.id.as_str().into(),
            via_alighting_point_id: alighting.id.as_str().into(),
            service_label: route.label.clone(),
            headsign: pattern.headsign.clone(),
            intermediate_stops,
        },
        attribution: Vec::new(),
    })
}

pub(super) fn transit_place(stop: &crate::domain::transit::network::TransitStop) -> Place {
    Place {
        display_name: stop.name.clone(),
        coordinate: stop.coordinate,
        via_id: Some(stop.id.as_str().into()),
    }
}

pub(super) fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> Option<u32> {
    u32::try_from((to - from).whole_seconds()).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::{Duration, OffsetDateTime, Time, macros::datetime};

    use super::super::JourneyReconstructor;
    use crate::{
        domain::{
            bikeshare::BikeShareNetwork,
            ids::{BoardingPointId, EffectiveTripId, TransitPatternId},
            journey::{JourneyOption, LegMode},
            location::GeoCoordinate,
            transit::{
                network::{TransitMode, TransitNetwork, TransitPattern, TransitStop},
                runtime::TransitRuntime,
                timing::{EffectiveStopTime, EffectiveTrip, TimingBasis, TimingReliability},
            },
        },
        routing::{
            JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
            PlanningDeadline, RealizedJourney, WalkingMovementKind, model::JourneyPlanningContext,
        },
        street::{Coordinate, FakeStreetRouter, Metres, Seconds, StreetPath, StreetTravelMode},
    };

    type Walk = (StreetTravelMode, Coordinate, Coordinate, StreetPath);

    const START: OffsetDateTime = datetime!(2026-09-21 10:00 +08:00);
    const ORIGIN: (f64, f64) = (25.0, 121.499);
    const BOARD_STOP: (f64, f64) = (25.0, 121.5);
    const BOARD_MATE: (f64, f64) = (25.0008, 121.5);
    const ALIGHT_STOP: (f64, f64) = (25.02, 121.5);
    const ALIGHT_MATE: (f64, f64) = (25.0208, 121.5);
    const DESTINATION: (f64, f64) = (25.02, 121.501);

    fn coordinate((latitude, longitude): (f64, f64)) -> Coordinate {
        Coordinate::try_new(latitude, longitude).unwrap()
    }

    fn geo((latitude, longitude): (f64, f64)) -> GeoCoordinate {
        GeoCoordinate {
            latitude,
            longitude,
        }
    }

    fn stop(id: &str, name: &str, position: (f64, f64)) -> TransitStop {
        TransitStop::new(BoardingPointId::new(id), name, geo(position))
    }

    fn walk(from: (f64, f64), to: (f64, f64), seconds: u32) -> Walk {
        (
            StreetTravelMode::Walking,
            coordinate(from),
            coordinate(to),
            StreetPath {
                duration: Seconds(seconds),
                distance: Metres(100),
                geometry: vec![coordinate(from), coordinate(to)],
                maneuvers: Vec::new(),
                attribution: Vec::new(),
            },
        )
    }

    fn ride(departure_seconds: i64, arrival_seconds: i64) -> Arc<EffectiveTrip> {
        let stop_time = |id: &str, seconds: i64| EffectiveStopTime {
            stop_id: BoardingPointId::new(id),
            arrival_at: START + Duration::seconds(seconds),
            departure_at: START + Duration::seconds(seconds),
        };
        Arc::new(EffectiveTrip {
            id: EffectiveTripId::new("ride"),
            pattern_id: TransitPatternId::new("bus-b"),
            scheduled_trip_id: None,
            stop_times: vec![
                stop_time("o2", departure_seconds),
                stop_time("d2", arrival_seconds),
            ],
            timing_basis: TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            evidence: Vec::new(),
        })
    }

    async fn reconstruct_place_mate_ride(
        trip: Arc<EffectiveTrip>,
        walks: &[Walk],
    ) -> Option<JourneyOption> {
        let stops = vec![
            stop("origin", "origin", BOARD_STOP),
            stop("o2", "origin", BOARD_MATE),
            stop("destination", "destination", ALIGHT_STOP),
            stop("d2", "destination", ALIGHT_MATE),
        ];
        let pattern = TransitPattern::fixture(
            TransitPatternId::new("bus-b"),
            TransitMode::Bus,
            "bus-b",
            "destination",
            vec![BoardingPointId::new("o2"), BoardingPointId::new("d2")],
        );
        let transit = Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::fixture(
            Time::MIDNIGHT,
            stops,
            vec![pattern],
            Vec::new(),
        ))));
        let mut street = FakeStreetRouter::builder();
        for (mode, from, to, path) in walks {
            street = street.path(*mode, *from, *to, path.clone());
        }
        let reconstructor = JourneyReconstructor::new(
            Arc::new(street.build()),
            transit,
            Arc::new(BikeShareNetwork::new(Vec::new())),
        );
        let request = serde_json::from_value::<crate::api::PlanningRequest>(serde_json::json!({
            "origin": {"coordinate": geo(ORIGIN), "display_text": "origin"},
            "destination": {"coordinate": geo(DESTINATION), "display_text": "destination"},
            "selected_modes": [], "departure_setting": {"type": "depart_now"}
        }))
        .unwrap();
        let request: crate::domain::planning::PlanningInput = request.into();
        let context = JourneyPlanningContext::new(&request, START, PlanningDeadline::disabled());
        let anchor = |kind, seconds: i64| JourneyAnchor {
            kind,
            reached_at: START + Duration::seconds(seconds),
        };
        let stop_anchor = |id: &str| JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new(id),
        };
        let arrival = trip.stop_times[1].arrival_at;
        let departure = trip.stop_times[0].departure_at;
        let walking = |from_anchor, kind, from: OffsetDateTime, seconds: i64| JourneyMovement {
            from_anchor,
            to_anchor: from_anchor + 1,
            departure_at: from,
            arrival_at: from + Duration::seconds(seconds),
            kind: JourneyMovementKind::Walking {
                kind,
                duration_seconds: seconds as u32,
                distance_metres: 100,
            },
        };
        let journey = RealizedJourney::from_movements(
            vec![
                anchor(JourneyAnchorKind::Origin, 0),
                anchor(stop_anchor("origin"), 60),
                anchor(stop_anchor("destination"), 1000),
                anchor(JourneyAnchorKind::Destination, 1060),
            ],
            vec![
                walking(0, WalkingMovementKind::Access, START, 60),
                JourneyMovement {
                    from_anchor: 1,
                    to_anchor: 2,
                    departure_at: departure,
                    arrival_at: arrival + Duration::seconds(100),
                    kind: JourneyMovementKind::Transit {
                        trip,
                        board_index: 0,
                        alight_index: 1,
                        board_hop_seconds: 120,
                        alight_hop_seconds: 100,
                    },
                },
                walking(
                    2,
                    WalkingMovementKind::Egress,
                    arrival + Duration::seconds(100),
                    60,
                ),
            ],
            START,
        )
        .unwrap();
        reconstructor
            .reconstruct(&journey, &context)
            .await
            .unwrap()
            .map(|candidate| candidate.option)
    }

    fn hop_walks(board_seconds: u32, alight_seconds: u32) -> Vec<Walk> {
        vec![
            walk(ORIGIN, BOARD_STOP, 60),
            walk(BOARD_STOP, BOARD_MATE, board_seconds),
            walk(ALIGHT_MATE, ALIGHT_STOP, alight_seconds),
            walk(ALIGHT_STOP, DESTINATION, 60),
        ]
    }

    #[tokio::test]
    async fn boarding_a_place_mate_stop_is_rejected_when_the_routed_walk_outlasts_the_departure() {
        let option = reconstruct_place_mate_ride(ride(200, 900), &hop_walks(150, 100)).await;

        assert!(option.is_none());
    }

    #[tokio::test]
    async fn boarding_a_place_mate_stop_after_the_walk_draws_the_hop() {
        let option = reconstruct_place_mate_ride(ride(200, 900), &hop_walks(120, 100))
            .await
            .unwrap();

        let modes = option.legs.iter().map(|leg| leg.mode).collect::<Vec<_>>();
        assert_eq!(
            modes,
            [LegMode::Walking, LegMode::Transit, LegMode::Walking]
        );
        assert_eq!(option.legs[0].duration_seconds, 180);
        assert_eq!(option.legs[0].to.via_id.as_deref(), Some("o2"));
        assert_eq!(option.legs[1].from.via_id.as_deref(), Some("o2"));
        assert_eq!(option.legs[1].to.via_id.as_deref(), Some("d2"));
        assert_eq!(option.legs[2].from.via_id.as_deref(), Some("d2"));
        assert_eq!(option.legs[2].duration_seconds, 160);
        assert_eq!(
            option.arrival_time,
            START + Duration::seconds(900 + 100 + 60)
        );
        assert_eq!(option.walking_seconds, 60 + 120 + 100 + 60);
    }
}
