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
            street_path: None,
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

pub(super) struct BikeShareExpander;

#[async_trait::async_trait]
impl MovementExpander for BikeShareExpander {
    async fn extend_state(
        &mut self,
        context: &mut ExpansionContext<'_>,
        state: &EvaluationState,
        next: &mut Vec<EvaluationState>,
    ) -> Result<(), RoutingError> {
        let (start_anchor, started_at) = state.bike_departure(context.index);
        let request = SurfaceRequest {
            from: &context.corridor.anchors()[start_anchor],
            to: &context.corridor.anchors()[context.index + 1],
            departure_at: started_at,
            from_anchor: start_anchor,
            to_anchor: context.index + 1,
        };
        for planner_index in 0..context.surface_planners.len() {
            let planner = &context.surface_planners[planner_index];
            if planner.kind() != SurfaceKind::BikeShare {
                continue;
            }
            let Some(bound) = planner.optimistic_duration(&request) else {
                continue;
            };
            context.stats.bike_lower_bound_checks += 1;
            if !within_query_bounds(
                context.query,
                started_at + Duration::seconds(i64::from(bound)),
            ) {
                context.stats.bike_lower_bound_rejections += 1;
                continue;
            }
            for movement in context.realize(planner_index, &request).await? {
                let SurfaceMovement::BikeShare(movement) = movement else {
                    continue;
                };
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
        }
        Ok(())
    }
}
