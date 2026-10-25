use super::expansion::{ExpansionContext, MovementExpander};
use super::*;

pub(super) struct WalkingExpander;

#[async_trait::async_trait]
impl MovementExpander for WalkingExpander {
    async fn extend_state(
        &mut self,
        context: &mut ExpansionContext<'_>,
        state: &EvaluationState,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let mut next = Vec::new();
        let index = context.index;
        if let PlannedMovement::RetainedWalking {
            kind,
            duration_seconds,
            distance_metres,
            access_available_at,
        } = &context.edge.planned_movement
        {
            context.charge()?;
            let arrival = access_available_at
                .unwrap_or(state.ready_at + Duration::seconds(i64::from(*duration_seconds)));
            if within_query_bounds(context.query, arrival) {
                next.push(state.after_movement(JourneyMovement {
                    from_anchor: index,
                    to_anchor: index + 1,
                    departure_at: state.ready_at,
                    arrival_at: arrival,
                    kind: JourneyMovementKind::Walking {
                        kind: *kind,
                        duration_seconds: *duration_seconds,
                        distance_metres: *distance_metres,
                    },
                    street_path: None,
                }));
            }
            return Ok(next);
        }
        let request = SurfaceRequest {
            from: &context.corridor.anchors()[index],
            to: &context.corridor.anchors()[index + 1],
            departure_at: state.ready_at,
            from_anchor: index,
            to_anchor: index + 1,
        };
        for planner_index in 0..context.surface_planners.len() {
            let planner = &context.surface_planners[planner_index];
            if planner.kind() != SurfaceKind::Walking {
                continue;
            }
            let Some(bound) = planner.optimistic_duration(&request) else {
                continue;
            };
            if !within_query_bounds(
                context.query,
                state.ready_at + Duration::seconds(i64::from(bound)),
            ) {
                continue;
            }
            for movement in context.realize(planner_index, &request).await? {
                let SurfaceMovement::Walking(path) = movement else {
                    continue;
                };
                let arrival = state.ready_at + Duration::seconds(i64::from(path.duration.0));
                if !within_query_bounds(context.query, arrival) {
                    continue;
                }
                let kind = match (&request.from.kind, &request.to.kind) {
                    (JourneyAnchorKind::Origin, JourneyAnchorKind::Destination) => {
                        WalkingMovementKind::Direct
                    }
                    (JourneyAnchorKind::Origin, _) => WalkingMovementKind::Access,
                    (_, JourneyAnchorKind::Destination) => WalkingMovementKind::Egress,
                    _ => WalkingMovementKind::Transfer,
                };
                next.push(state.after_movement(JourneyMovement {
                    from_anchor: index,
                    to_anchor: index + 1,
                    departure_at: state.ready_at,
                    arrival_at: arrival,
                    kind: JourneyMovementKind::Walking {
                        kind,
                        duration_seconds: path.duration.0,
                        distance_metres: path.distance.0,
                    },
                    street_path: Some(path),
                }));
            }
        }
        Ok(next)
    }
}
