use super::*;

impl FixedCorridorEvaluator<'_> {
    pub(super) fn expand_transit(
        &mut self,
        state: &EvaluationState,
        leg: &StaticTransitLeg,
        saw_missing_departure: &mut bool,
    ) -> Result<ControlFlow<(), Vec<EvaluationState>>, RoutingError> {
        let mut normal_successors = Vec::new();

        let alternatives = self.corridor.equivalent_legs_for(self.network, leg);
        for alternative in &alternatives {
            if self
                .query
                .deadline
                .is_some_and(|deadline| deadline.is_elapsed())
            {
                return Err(RoutingError::DeadlineExceeded);
            }
            if charge_state(&mut self.stats, self.query.limits.max_phase2_expansions) {
                self.stats.expansion_limit_reached = true;
                return Ok(ControlFlow::Break(()));
            }
            if !valid_pattern_leg(self.network, self.corridor, leg, alternative) {
                continue;
            }
            let boarding_ready_at = if state.last_was_transit {
                state.ready_at
                    + Duration::seconds(i64::from(self.network.same_stop_transfer_seconds()))
            } else {
                state.ready_at
            };
            if !within_query_bounds(self.query, boarding_ready_at) {
                continue;
            }
            self.stats.transit_departure_lookups += 1;
            let departures = self.snapshot.departures(
                &alternative.pattern_id,
                alternative.board_index,
                boarding_ready_at,
                self.query.limits.max_departures_per_query,
            );
            if departures.is_empty() {
                *saw_missing_departure = true;
            }
            self.stats.effective_trips_examined += departures.len();
            for trip in departures {
                if self
                    .query
                    .deadline
                    .is_some_and(|deadline| deadline.is_elapsed())
                {
                    return Err(RoutingError::DeadlineExceeded);
                }
                let Some(arrival_at) =
                    retained_alight_time(self.network, trip, alternative, boarding_ready_at)
                else {
                    self.stats.malformed_trips += 1;
                    continue;
                };
                if !within_query_bounds(self.query, arrival_at)
                    || self
                        .query
                        .latest_arrival
                        .is_some_and(|latest| arrival_at > latest)
                {
                    continue;
                }
                normal_successors.push(
                    state.after_normal_transition(
                        alternative.alight_stop_id.clone(),
                        arrival_at,
                        state.transit_legs + 1,
                        true,
                        state.path.with_ride(
                            trip,
                            alternative.board_index,
                            alternative.alight_index,
                        ),
                    ),
                );
            }
        }

        Ok(ControlFlow::Continue(normal_successors))
    }
}

fn valid_pattern_leg(
    network: &TransitRuntime,
    corridor: &RetainedCorridorSet,
    retained: &StaticTransitLeg,
    alternative: &StaticTransitLeg,
) -> bool {
    if !corridor.is_equivalent_leg(network, retained, alternative) {
        return false;
    }
    let Some(pattern) = network.pattern(&alternative.pattern_id) else {
        return false;
    };
    pattern.stops.get(alternative.board_index) == Some(&retained.board_stop_id)
        && pattern.stops.get(alternative.alight_index) == Some(&retained.alight_stop_id)
        && alternative.board_index < alternative.alight_index
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
