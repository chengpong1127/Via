use super::*;

impl FixedCorridorEvaluator<'_> {
    pub(super) async fn expand_bike(
        &mut self,
        state: &EvaluationState,
        from_anchor: usize,
        anchor_stops: &[BoardingPointId],
        best_normal_arrival: Option<OffsetDateTime>,
        successors: &mut Vec<EvaluationState>,
    ) -> Result<ControlFlow<()>, RoutingError> {
        #[cfg(test)]
        let test_movement = self.bike_realizations.and_then(|realizations| {
            let start_anchor = state
                .active_bike_run
                .as_ref()
                .map_or(from_anchor, |run| run.start_anchor);
            let from = state.active_bike_run.as_ref().map_or_else(
                || anchor_stops[from_anchor].clone(),
                |run| run.start_stop_id.clone(),
            );
            let movement = CorridorMovement {
                from_anchor: start_anchor,
                to_anchor: from_anchor + 1,
                from,
                to: anchor_stops[from_anchor + 1].clone(),
            };
            realizations
                .get(&movement)
                .map(|shortcuts| (movement, shortcuts))
        });

        if let Some((bike_start_anchor, bike_start_stop, bike_started_at, prefix_path)) =
            bike_start(state, from_anchor)
        {
            #[cfg(test)]
            let test_lower_bound = test_movement.as_ref().and_then(|(_, shortcuts)| {
                shortcuts
                    .iter()
                    .map(|shortcut| shortcut.total_seconds / 2)
                    .min()
            });
            #[cfg(test)]
            let candidate = self.bike_context.and_then(|(planner, availability, now)| {
                planner.movement_candidate(
                    &bike_start_stop,
                    &anchor_stops[from_anchor + 1],
                    availability,
                    now,
                )
            });
            #[cfg(not(test))]
            let candidate = self.bike_context.and_then(|(planner, availability, now)| {
                planner.movement_candidate(
                    &bike_start_stop,
                    &anchor_stops[from_anchor + 1],
                    availability,
                    now,
                )
            });
            #[cfg(test)]
            let lower_bound_seconds = candidate
                .as_ref()
                .map(|candidate| candidate.optimistic_lower_bound_seconds)
                .or(test_lower_bound);
            #[cfg(not(test))]
            let lower_bound_seconds = candidate
                .as_ref()
                .map(|candidate| candidate.optimistic_lower_bound_seconds);

            if let Some(lower_bound_seconds) = lower_bound_seconds {
                self.stats.bike_lower_bound_checks += 1;
                let optimistic_arrival =
                    bike_started_at + Duration::seconds(i64::from(lower_bound_seconds));
                if best_normal_arrival.is_some_and(|normal| optimistic_arrival >= normal) {
                    self.stats.bike_lower_bound_rejections += 1;
                } else if !charge_state(&mut self.stats, self.query.limits.max_phase2_expansions) {
                    let route_key = (
                        bike_start_stop.clone(),
                        anchor_stops[from_anchor + 1].clone(),
                    );
                    let mut bike_options =
                        if let Some(cached) = self.bike_route_cache.get(&route_key) {
                            self.stats.bike_exact_cache_hits += 1;
                            cached.clone()
                        } else {
                            self.stats.bike_exact_candidates_requested += 1;
                            #[cfg(test)]
                            let mut exact = if let (Some((planner, _, _)), Some(candidate)) =
                                (self.bike_context, candidate.as_ref())
                            {
                                let (items, bike_stats) =
                                    planner.realize_candidate(candidate, 0, 1).await;
                                if self
                                    .query
                                    .deadline
                                    .is_some_and(|deadline| deadline.is_elapsed())
                                {
                                    return Err(RoutingError::DeadlineExceeded);
                                }
                                self.stats.bike_matrix_requests += bike_stats.matrix_requests;
                                items
                            } else {
                                test_movement
                                    .as_ref()
                                    .map(|(_, shortcuts)| shortcuts.to_vec())
                                    .unwrap_or_default()
                            };
                            #[cfg(not(test))]
                            let mut exact = if let (Some((planner, _, _)), Some(candidate)) =
                                (self.bike_context, candidate.as_ref())
                            {
                                let (items, bike_stats) =
                                    planner.realize_candidate(candidate, 0, 1).await;
                                if self
                                    .query
                                    .deadline
                                    .is_some_and(|deadline| deadline.is_elapsed())
                                {
                                    return Err(RoutingError::DeadlineExceeded);
                                }
                                self.stats.bike_matrix_requests += bike_stats.matrix_requests;
                                items
                            } else {
                                Vec::new()
                            };
                            for shortcut in &mut exact {
                                shortcut.from_anchor = 0;
                                shortcut.to_anchor = 1;
                            }
                            self.bike_route_cache
                                .insert(route_key.clone(), exact.clone());
                            exact
                        };
                    for bike in &mut bike_options {
                        bike.from_anchor = bike_start_anchor;
                        bike.to_anchor = from_anchor + 1;
                    }
                    for bike in bike_options {
                        self.stats.bike_realizations_considered += 1;
                        let route_seconds = bike.total_seconds;
                        let rental_wait_seconds = state.active_bike_run.as_ref().map_or_else(
                            || {
                                u32::try_from((bike_started_at - state.ready_at).whole_seconds())
                                    .unwrap_or(u32::MAX)
                            },
                            |run| run.shortcut.rental_wait_seconds,
                        );
                        let Some(bike) = bike.with_rental_wait(rental_wait_seconds) else {
                            continue;
                        };
                        let ready_at =
                            bike_started_at + Duration::seconds(i64::from(route_seconds));
                        if best_normal_arrival.is_some_and(|normal| ready_at >= normal)
                            || !within_query_bounds(self.query, ready_at)
                        {
                            continue;
                        }
                        self.stats.bike_realizations_feasible += 1;
                        let (path, active_bike_run) = if let Some(run) = &state.active_bike_run {
                            self.stats.bike_continuous_extensions += 1;
                            (
                                run.prefix_path.with_bike_transfer(
                                    &anchor_stops[bike_start_anchor],
                                    &anchor_stops[from_anchor + 1],
                                    &bike,
                                ),
                                ActiveBikeRun {
                                    start_anchor: run.start_anchor,
                                    start_stop_id: run.start_stop_id.clone(),
                                    started_at: run.started_at,
                                    prefix_path: run.prefix_path.clone(),
                                    shortcut: bike.clone(),
                                },
                            )
                        } else {
                            (
                                state.path.with_bike_transfer(
                                    &anchor_stops[bike_start_anchor],
                                    &anchor_stops[from_anchor + 1],
                                    &bike,
                                ),
                                ActiveBikeRun {
                                    start_anchor: from_anchor,
                                    start_stop_id: anchor_stops[from_anchor].clone(),
                                    started_at: bike_started_at,
                                    prefix_path: prefix_path.clone(),
                                    shortcut: bike.clone(),
                                },
                            )
                        };
                        successors.push(EvaluationState {
                            stop_id: anchor_stops[from_anchor + 1].clone(),
                            ready_at,
                            transit_legs: state.transit_legs,
                            last_was_transit: false,
                            active_bike_run: Some(active_bike_run),
                            bike_reuse_available_at: state.bike_reuse_available_at,
                            path,
                        });
                    }
                } else {
                    self.stats.expansion_limit_reached = true;
                    return Ok(ControlFlow::Break(()));
                }
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}
