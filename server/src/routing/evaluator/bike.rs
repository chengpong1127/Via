use super::*;

#[derive(Clone)]
pub(super) struct ActiveBikeRental {
    pub start_anchor: usize,
    pub started_at: OffsetDateTime,
    pub prefix_movements: Vec<JourneyMovement>,
    pub movement: BikeShareMovement,
}
impl ActiveBikeRental {
    pub fn reuse_available_at(&self) -> OffsetDateTime {
        self.started_at
            + Duration::seconds(i64::from(
                self.movement
                    .walk_to_pickup
                    .duration_seconds
                    .saturating_add(self.movement.bicycle_duration_seconds),
            ))
            + Duration::minutes(15)
    }
}
impl EvaluationState {
    pub fn bike_departure(&self, index: usize) -> (usize, OffsetDateTime) {
        self.active_bike_rental.as_ref().map_or_else(
            || {
                (
                    index,
                    self.bike_reuse_available_at
                        .map_or(self.ready_at, |time| time.max(self.ready_at)),
                )
            },
            |rental| (rental.start_anchor, rental.started_at),
        )
    }
    pub fn after_bike_share(
        &self,
        movement: BikeShareMovement,
        start_anchor: usize,
        to_anchor: usize,
        started_at: OffsetDateTime,
        arrival: OffsetDateTime,
        query_departure: OffsetDateTime,
    ) -> Option<Self> {
        let wait = self.active_bike_rental.as_ref().map_or_else(
            || u32::try_from((started_at - self.ready_at).whole_seconds()).unwrap_or(u32::MAX),
            |rental| rental.movement.rental_wait_seconds,
        );
        let movement = movement.with_rental_wait(wait)?;
        let prefix = self.active_bike_rental.as_ref().map_or_else(
            || self.movements.clone(),
            |rental| rental.prefix_movements.clone(),
        );
        let departure = prefix
            .last()
            .map_or(query_departure, |movement| movement.arrival_at);
        let mut movements = prefix.clone();
        movements.push(JourneyMovement {
            from_anchor: start_anchor,
            to_anchor,
            departure_at: departure,
            arrival_at: arrival,
            kind: JourneyMovementKind::BikeShare {
                movement: movement.clone(),
            },
        });
        Some(Self {
            ready_at: arrival,
            last_was_transit: false,
            active_bike_rental: Some(ActiveBikeRental {
                start_anchor,
                started_at,
                prefix_movements: prefix,
                movement,
            }),
            bike_reuse_available_at: self.bike_reuse_available_at,
            movements,
        })
    }
}

use super::expansion::{ExpansionContext, MovementExpander};

pub(super) struct BikeShareExpander<'a> {
    pub session: Option<&'a mut super::super::bike::BikeShareSession>,
}

#[async_trait::async_trait]
impl MovementExpander for BikeShareExpander<'_> {
    async fn extend_state(
        &mut self,
        context: &mut ExpansionContext<'_>,
        state: &EvaluationState,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let mut next = Vec::new();
        let (start_anchor, started_at) = state.bike_departure(context.index);
        let request = super::super::bike::BikeMovementRequest {
            from: &context.corridor.anchors()[start_anchor],
            to: &context.corridor.anchors()[context.index + 1],
            from_anchor: start_anchor,
            to_anchor: context.index + 1,
        };
        let Some(planner) = self.session.as_deref_mut() else {
            return Ok(next);
        };
        let Some(candidate) = planner.candidate(&request) else {
            return Ok(next);
        };
        let bound = candidate.optimistic_lower_bound_seconds;
        context.stats.bike_lower_bound_checks += 1;
        if !within_query_bounds(
            context.query,
            started_at + Duration::seconds(i64::from(bound)),
        ) {
            context.stats.bike_lower_bound_rejections += 1;
            return Ok(next);
        }
        context.charge()?;
        let result = planner.realize(&request, &candidate).await;
        for movement in context.finish_realization(result)?.unwrap_or_default() {
            let arrival = started_at + Duration::seconds(i64::from(movement.total_seconds));
            if !within_query_bounds(context.query, arrival) {
                continue;
            }
            if let Some(successor) = state.after_bike_share(
                movement,
                start_anchor,
                context.index + 1,
                started_at,
                arrival,
                context.query.departure_time,
            ) {
                next.push(successor);
            }
        }
        Ok(next)
    }
}
