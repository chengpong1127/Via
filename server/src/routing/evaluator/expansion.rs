use super::*;

impl FixedCorridorEvaluator<'_> {
    pub(super) async fn expand_state(
        &mut self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
        next: &mut Vec<EvaluationState>,
    ) -> Result<(), RoutingError> {
        self.check_deadline()?;
        let baseline = self.expand_baselines(state, edge, index)?;
        let best_baseline_arrival = baseline.iter().map(|s| s.ready_at).min();
        next.extend(baseline);
        self.expand_surface_alternatives(state, corridor, edge, index, best_baseline_arrival, next)
            .await
    }

    fn expand_baselines(
        &mut self,
        state: &EvaluationState,
        edge: &CorridorEdge,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let mut baseline = Vec::new();
        for movement in &edge.baseline {
            match movement {
                BaselineMovement::Transit(leg) => {
                    baseline.extend(self.expand_transit(state, leg, index)?)
                }
                BaselineMovement::RetainedWalking {
                    kind,
                    duration_seconds,
                    distance_metres,
                    access_available_at,
                } => {
                    self.charge()?;
                    let arrival = access_available_at.unwrap_or(
                        state.ready_at + Duration::seconds(i64::from(*duration_seconds)),
                    );
                    if within_query_bounds(self.query, arrival) {
                        baseline.push(state.after_movement(JourneyMovement {
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
                }
            }
        }
        Ok(baseline)
    }

    async fn expand_surface_alternatives(
        &mut self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
        best_baseline_arrival: Option<OffsetDateTime>,
        next: &mut Vec<EvaluationState>,
    ) -> Result<(), RoutingError> {
        for planner_index in 0..self.surface_planners.len() {
            let kind = self.surface_planners[planner_index].kind();
            let (start_anchor, started_at) = if kind == SurfaceKind::BikeShare {
                state.bike_departure(index)
            } else {
                (index, state.ready_at)
            };
            let request = SurfaceRequest {
                retained_walking_baseline: edge
                    .baseline
                    .iter()
                    .any(|b| matches!(b, BaselineMovement::RetainedWalking { .. })),
                from: &corridor.anchors()[start_anchor],
                to: &corridor.anchors()[index + 1],
                departure_at: started_at,
                from_anchor: start_anchor,
                to_anchor: index + 1,
            };
            let Some(bound) = self.surface_planners[planner_index].optimistic_duration(&request)
            else {
                continue;
            };
            if kind == SurfaceKind::BikeShare {
                self.stats.bike_lower_bound_checks += 1;
            }
            let optimistic_arrival = started_at + Duration::seconds(i64::from(bound));
            if best_baseline_arrival.is_some_and(|arrival| optimistic_arrival >= arrival)
                || !within_query_bounds(self.query, optimistic_arrival)
            {
                if kind == SurfaceKind::BikeShare {
                    self.stats.bike_lower_bound_rejections += 1;
                }
                continue;
            }
            self.charge()?;
            let movements = self.surface_planners[planner_index].realize(&request).await;
            self.check_deadline()?;
            let movements = match movements {
                Ok(items) => items,
                Err(error) => {
                    match error.failure_class() {
                        ProviderFailureClass::Transient => self.failures.transient = true,
                        ProviderFailureClass::Internal => self.failures.internal = true,
                        ProviderFailureClass::Unreachable => {}
                    }
                    continue;
                }
            };
            for movement in movements {
                match movement {
                    SurfaceMovement::Walking(path) => {
                        let arrival =
                            state.ready_at + Duration::seconds(i64::from(path.duration.0));
                        if !within_query_bounds(self.query, arrival) {
                            continue;
                        }

                        let kind = match (
                            &corridor.anchors()[index].kind,
                            &corridor.anchors()[index + 1].kind,
                        ) {
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
                    SurfaceMovement::BikeShare(bike_movement) => {
                        let route_seconds = bike_movement.total_seconds;
                        let arrival = started_at + Duration::seconds(i64::from(route_seconds));
                        if best_baseline_arrival.is_some_and(|baseline| arrival >= baseline)
                            || !within_query_bounds(self.query, arrival)
                        {
                            continue;
                        }
                        if let Some(next_state) = state.after_bike_share(
                            bike_movement,
                            start_anchor,
                            index + 1,
                            started_at,
                            arrival,
                            self.query.departure_time,
                        ) {
                            next.push(next_state);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
