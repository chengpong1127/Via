use super::*;

impl FixedCorridorEvaluator<'_> {
    pub(super) fn expand_transit(
        &mut self,
        state: &EvaluationState,
        leg: &StaticTransitLeg,
        anchor: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let mut successors = Vec::new();
        for equivalent_leg in self.corridor.equivalent_legs_for(self.network, leg) {
            self.charge()?;
            valid_pattern_leg(self.network, self.corridor, leg, &equivalent_leg)?;
            let ready_at = if state.last_was_transit {
                state.ready_at
                    + Duration::seconds(i64::from(self.network.same_stop_transfer_seconds()))
            } else {
                state.ready_at
            };
            if !within_query_bounds(self.query, ready_at) {
                continue;
            }
            self.stats.transit_departure_lookups += 1;
            let departures = self.snapshot.departures(
                &equivalent_leg.pattern_id,
                equivalent_leg.board_index,
                ready_at,
                self.query.limits.max_departures_per_query,
            );
            for trip in departures {
                self.check_deadline()?;
                let Some(arrival) =
                    retained_alight_time(self.network, trip, &equivalent_leg, ready_at)
                else {
                    continue;
                };
                if !within_query_bounds(self.query, arrival) {
                    continue;
                }
                successors.push(state.after_movement(JourneyMovement {
                    from_anchor: anchor,
                    to_anchor: anchor + 1,
                    departure_at: trip.stop_times[equivalent_leg.board_index].departure_at,
                    arrival_at: arrival,
                    kind: JourneyMovementKind::Transit {
                        trip: Arc::new(trip.clone()),
                        board_index: equivalent_leg.board_index,
                        alight_index: equivalent_leg.alight_index,
                    },
                    street_path: None,
                }));
            }
        }
        Ok(successors)
    }
}
fn valid_pattern_leg(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    retained: &StaticTransitLeg,
    equivalent_leg: &StaticTransitLeg,
) -> Result<(), RoutingError> {
    if !corridor.is_equivalent_leg(network, retained, equivalent_leg) {
        return Err(RoutingError::Internal(
            "retained transit leg has an invalid equivalent leg".into(),
        ));
    }
    if equivalent_leg.board_index >= equivalent_leg.alight_index {
        return Err(RoutingError::Internal(
            "retained transit leg has reversed stop indexes".into(),
        ));
    }
    let pattern = network.required_pattern(&equivalent_leg.pattern_id);
    let board_stop = pattern
        .stops
        .get(equivalent_leg.board_index)
        .ok_or_else(|| {
            RoutingError::Internal("retained transit leg has an invalid boarding index".into())
        })?;
    let alight_stop = pattern
        .stops
        .get(equivalent_leg.alight_index)
        .ok_or_else(|| {
            RoutingError::Internal("retained transit leg has an invalid alighting index".into())
        })?;
    if board_stop != &retained.board_stop_id || alight_stop != &retained.alight_stop_id {
        return Err(RoutingError::Internal(
            "retained transit leg indexes do not match its stops".into(),
        ));
    }
    Ok(())
}

fn retained_alight_time(
    network: &TransitRuntime,
    trip: &EffectiveTrip,
    leg: &StaticTransitLeg,
    ready_at: OffsetDateTime,
) -> Option<OffsetDateTime> {
    if trip.pattern_id != leg.pattern_id || leg.board_index >= leg.alight_index {
        return None;
    }
    let pattern = network.pattern(&leg.pattern_id)?;
    if trip.stop_times.len() != pattern.stops.len()
        || trip
            .stop_times
            .iter()
            .zip(&pattern.stops)
            .any(|(time, stop_id)| &time.stop_id != stop_id)
        || trip
            .stop_times
            .iter()
            .any(|time| time.arrival_at > time.departure_at)
        || trip
            .stop_times
            .windows(2)
            .any(|pair| pair[0].departure_at > pair[1].arrival_at)
    {
        return None;
    }
    let board = trip.stop_times.get(leg.board_index)?;
    let alight = trip.stop_times.get(leg.alight_index)?;
    if board.stop_id != leg.board_stop_id
        || alight.stop_id != leg.alight_stop_id
        || board.departure_at < ready_at
        || alight.arrival_at < board.departure_at
    {
        return None;
    }
    Some(alight.arrival_at)
}
