use super::*;
use crate::{
    domain::normalize_to_taipei,
    routing::corridor::CorridorAnchorKey,
    street::{
        MatrixCell, MatrixOrientation, MatrixQuery, RoutingConstraints, RoutingTime,
        RoutingTimeKind, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};
use std::collections::HashMap;

pub(crate) struct WalkingCache {
    street: Arc<dyn StreetRouter>,
    cache: HashMap<
        (CorridorAnchorKey, CorridorAnchorKey, OffsetDateTime),
        Result<MatrixCell, StreetRoutingError>,
    >,
    pub requests: usize,
    pub cache_hits: usize,
}

impl WalkingCache {
    pub fn new(street: Arc<dyn StreetRouter>) -> Self {
        Self {
            street,
            cache: HashMap::new(),
            requests: 0,
            cache_hits: 0,
        }
    }

    pub(super) async fn realize(
        &mut self,
        from: &super::super::corridor::CorridorAnchor,
        to: &super::super::corridor::CorridorAnchor,
        departure_at: OffsetDateTime,
    ) -> Result<MatrixCell, StreetRoutingError> {
        let key = (from.cache_key(), to.cache_key(), departure_at);
        if let Some(result) = self.cache.get(&key) {
            self.cache_hits += 1;
            return result.clone();
        }
        self.requests += 1;
        let local = normalize_to_taipei(departure_at);
        let result = self
            .street
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![from.coordinate],
                targets: vec![to.coordinate],
                constraints: RoutingConstraints {
                    time: Some(RoutingTime {
                        kind: RoutingTimeKind::DepartAt,
                        local_date_time: local.date().with_time(local.time()),
                    }),
                    max_distance: None,
                },
            })
            .await
            .and_then(|matrix| {
                if matrix.cells.len() != 1 || matrix.cells[0].len() != 1 {
                    return Err(StreetRoutingError::MalformedResponse);
                }
                Ok(matrix.cells[0][0])
            });
        self.cache.insert(key, result.clone());
        result
    }
}

impl FixedCorridorEvaluator<'_> {
    pub(super) async fn expand_state(
        &mut self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        self.check_deadline()?;
        let mut next = self.expand_transit(state, edge, index)?;
        self.check_deadline()?;
        next.extend(self.expand_walking(state, corridor, edge, index).await?);
        self.check_deadline()?;
        next.extend(self.expand_bike_share(state, corridor, index).await?);
        Ok(next)
    }
    fn charge(&mut self) -> Result<(), RoutingError> {
        self.check_deadline()?;
        if self.stats.corridor_states_evaluated >= self.query.limits.max_phase2_expansions {
            return Err(RoutingError::Phase2EvaluationLimit);
        }
        self.stats.corridor_states_evaluated += 1;
        Ok(())
    }

    fn finish_realization<T>(
        &mut self,
        result: Result<T, crate::street::StreetRoutingError>,
    ) -> Result<Option<T>, RoutingError> {
        self.check_deadline()?;
        match result {
            Ok(items) => Ok(Some(items)),
            Err(error) => {
                match error.failure_class() {
                    ProviderFailureClass::Transient => self.failures.transient = true,
                    ProviderFailureClass::Internal => self.failures.internal = true,
                    ProviderFailureClass::Unreachable => {}
                }
                Ok(None)
            }
        }
    }

    fn expand_transit(
        &mut self,
        state: &EvaluationState,
        edge: &CorridorEdge,
        anchor: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let PlannedMovement::Transit(leg) = &edge.planned_movement else {
            return Ok(Vec::new());
        };
        let mut next = Vec::new();
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
                next.push(state.after_movement(JourneyMovement {
                    from_anchor: anchor,
                    to_anchor: anchor + 1,
                    departure_at: trip.stop_times[equivalent_leg.board_index].departure_at,
                    arrival_at: arrival,
                    kind: JourneyMovementKind::Transit {
                        trip: Arc::new(trip.clone()),
                        board_index: equivalent_leg.board_index,
                        alight_index: equivalent_leg.alight_index,
                    },
                }));
            }
        }
        Ok(next)
    }

    async fn expand_walking(
        &mut self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let mut next = Vec::new();
        if let PlannedMovement::RetainedWalking {
            kind,
            duration_seconds,
            distance_metres,
            access_available_at,
        } = &edge.planned_movement
        {
            self.charge()?;
            let arrival = access_available_at
                .unwrap_or(state.ready_at + Duration::seconds(i64::from(*duration_seconds)));
            if within_query_bounds(self.query, arrival) {
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
                }));
            }
            return Ok(next);
        }
        let from = &corridor.anchors()[index];
        let to = &corridor.anchors()[index + 1];
        self.charge()?;
        let result = self.walking.realize(from, to, state.ready_at).await;
        let (duration, distance) = match self.finish_realization(result)? {
            Some(MatrixCell::Reachable { duration, distance }) => (duration, distance),
            Some(MatrixCell::Unreachable) | None => return Ok(next),
        };
        let arrival = state.ready_at + Duration::seconds(i64::from(duration.0));
        if !within_query_bounds(self.query, arrival) {
            return Ok(next);
        }
        let kind = match (&from.kind, &to.kind) {
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
                duration_seconds: duration.0,
                distance_metres: distance.0,
            },
        }));
        Ok(next)
    }

    async fn expand_bike_share(
        &mut self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let mut next = Vec::new();
        let (start_anchor, started_at) = state.bike_departure(index);
        let request = super::super::bike::BikeMovementRequest {
            from: &corridor.anchors()[start_anchor],
            to: &corridor.anchors()[index + 1],
            from_anchor: start_anchor,
            to_anchor: index + 1,
        };
        let Some(planner) = self.bike_share.as_deref_mut() else {
            return Ok(next);
        };
        let Some(candidate) = planner.candidate(&request) else {
            return Ok(next);
        };
        self.charge()?;
        let result = match self.bike_share.as_mut() {
            Some(session) => session.realize(&request, &candidate).await,
            None => return Ok(next),
        };
        for movement in self.finish_realization(result)?.unwrap_or_default() {
            let arrival = started_at + Duration::seconds(i64::from(movement.total_seconds));
            if !within_query_bounds(self.query, arrival) {
                continue;
            }
            if let Some(successor) = state.after_bike_share(
                movement,
                start_anchor,
                index + 1,
                started_at,
                arrival,
                self.query.departure_time,
            ) {
                next.push(successor);
            }
        }
        Ok(next)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        routing::corridor::CorridorAnchor,
        street::{Coordinate, MatrixResult, RouteQuery, StreetPath},
    };
    use std::sync::Mutex;
    use time::macros::datetime;

    struct RecordingStreet {
        queries: Mutex<Vec<MatrixQuery>>,
        result: Result<MatrixCell, StreetRoutingError>,
    }

    #[async_trait::async_trait]
    impl StreetRouter for RecordingStreet {
        async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
            self.queries.lock().unwrap().push(query);
            self.result.clone().map(|cell| MatrixResult {
                cells: vec![vec![cell]],
            })
        }
        async fn route(&self, _: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
            panic!("Phase 2 must not fetch geometry")
        }
    }

    #[tokio::test]
    async fn cache_preserves_direction_time_coordinates_and_boarding_identity() {
        let street = Arc::new(RecordingStreet {
            queries: Mutex::new(Vec::new()),
            result: Ok(MatrixCell::Reachable {
                duration: crate::street::Seconds(60),
                distance: crate::street::Metres(90),
            }),
        });
        let mut walking = WalkingCache::new(street.clone());
        let from = CorridorAnchor {
            kind: JourneyAnchorKind::Origin,
            coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
        };
        let to = CorridorAnchor {
            kind: JourneyAnchorKind::Destination,
            coordinate: Coordinate::try_new(25.001, 121.501).unwrap(),
        };
        let departure = datetime!(2026-09-21 23:59 UTC);
        walking.realize(&from, &to, departure).await.unwrap();
        walking
            .realize(
                &from,
                &to,
                departure.to_offset(time::UtcOffset::from_hms(8, 0, 0).unwrap()),
            )
            .await
            .unwrap();
        walking.realize(&to, &from, departure).await.unwrap();
        walking
            .realize(&from, &to, departure + Duration::seconds(1))
            .await
            .unwrap();
        let boarding = CorridorAnchor {
            kind: JourneyAnchorKind::BoardingPoint {
                id: crate::domain::ids::BoardingPointId::new("a"),
            },
            ..from.clone()
        };
        walking.realize(&boarding, &to, departure).await.unwrap();
        let moved = CorridorAnchor {
            coordinate: Coordinate::try_new(25.00001, 121.5).unwrap(),
            ..from.clone()
        };
        walking.realize(&moved, &to, departure).await.unwrap();
        assert_eq!((walking.requests, walking.cache_hits), (5, 1));
        let mut other_request = WalkingCache::new(street.clone());
        other_request.realize(&from, &to, departure).await.unwrap();
        assert_eq!((other_request.requests, other_request.cache_hits), (1, 0));
        let queries = street.queries.lock().unwrap();
        assert_eq!(queries[0].sources, vec![from.coordinate]);
        assert_eq!(queries[0].targets, vec![to.coordinate]);
        assert_eq!(queries[1].sources, vec![to.coordinate]);
        assert_eq!(queries[1].targets, vec![from.coordinate]);
        assert_eq!(
            queries[0].constraints.time,
            Some(RoutingTime {
                kind: RoutingTimeKind::DepartAt,
                local_date_time: time::macros::datetime!(2026-09-22 07:59)
            })
        );
    }

    #[tokio::test]
    async fn cache_retains_unreachable_and_provider_failures() {
        for result in [
            Ok(MatrixCell::Unreachable),
            Err(StreetRoutingError::Timeout),
            Err(StreetRoutingError::MalformedResponse),
        ] {
            let street = Arc::new(RecordingStreet {
                queries: Mutex::new(Vec::new()),
                result: result.clone(),
            });
            let mut walking = WalkingCache::new(street.clone());
            let corridor = EvaluationCorridor::direct_walking(
                Coordinate::try_new(25.0, 121.5).unwrap(),
                Coordinate::try_new(25.01, 121.5).unwrap(),
            );
            for _ in 0..2 {
                assert_eq!(
                    walking
                        .realize(
                            &corridor.anchors()[0],
                            &corridor.anchors()[1],
                            datetime!(2026-09-21 10:00 UTC)
                        )
                        .await,
                    result
                );
            }
            assert_eq!((walking.requests, walking.cache_hits), (1, 1));
            assert_eq!(street.queries.lock().unwrap().len(), 1);
        }
    }
}
