use super::*;
use crate::routing::request_cache::RequestCache;
use crate::{
    domain::normalize_to_taipei,
    routing::corridor::CorridorAnchorKey,
    street::{
        MatrixCell, MatrixOrientation, MatrixQuery, RoutingConstraints, RoutingTime,
        RoutingTimeKind, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

pub(crate) struct WalkingCache {
    street: Arc<crate::routing::matrix::SharedMatrixResolver>,
    cache: RequestCache<
        (CorridorAnchorKey, CorridorAnchorKey, OffsetDateTime),
        Result<MatrixCell, StreetRoutingError>,
    >,
}

impl WalkingCache {
    #[cfg(test)]
    pub fn new(street: Arc<dyn StreetRouter>) -> Self {
        Self::with_resolver(Arc::new(crate::routing::matrix::SharedMatrixResolver::new(
            street,
        )))
    }

    pub fn with_resolver(street: Arc<crate::routing::matrix::SharedMatrixResolver>) -> Self {
        Self {
            street,
            cache: RequestCache::default(),
        }
    }

    pub fn matrix_stats(&self) -> crate::routing::MatrixResolverStats {
        self.street.stats()
    }

    pub(super) async fn realize(
        &self,
        from: &super::super::corridor::CorridorAnchor,
        to: &super::super::corridor::CorridorAnchor,
        departure_at: OffsetDateTime,
    ) -> Result<MatrixCell, StreetRoutingError> {
        let key = (from.cache_key(), to.cache_key(), departure_at);
        self.cache
            .get_or_init(key, || async {
                let local = normalize_to_taipei(departure_at);
                self.street
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
                    })
            })
            .await
    }

    pub fn requests(&self) -> usize {
        self.cache.stats().initializations
    }
    pub fn cache_hits(&self) -> usize {
        let stats = self.cache.stats();
        stats.ready_hits + stats.inflight_hits
    }
}

pub(super) struct ExpansionInputs<'a> {
    pub network: &'a TransitRuntime,
    pub corridor: &'a RetainedCorridorSet,
    pub snapshot: &'a TimingSnapshot,
    pub query: &'a PlanningQuery,
}

pub(super) struct ExpansionResolver<'a> {
    pub inputs: ExpansionInputs<'a>,
    pub walking: &'a WalkingCache,
    pub bike_share: Option<&'a BikeShareSession>,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct ExpansionStats {
    pub states_expanded: usize,
    pub transit_operations_attempted: usize,
    pub transit_departure_lookups: usize,
    pub walking_operations_attempted: usize,
    pub bike_operations_attempted: usize,
    pub successors_generated: usize,
}

pub(super) struct ExpansionOutcome {
    pub successors: Result<Vec<EvaluationState>, RoutingError>,
    pub stats: ExpansionStats,
    pub failures: EvaluationFailures,
}

impl ExpansionStats {
    pub fn merge(&mut self, other: Self) {
        self.states_expanded += other.states_expanded;
        self.transit_operations_attempted += other.transit_operations_attempted;
        self.transit_departure_lookups += other.transit_departure_lookups;
        self.walking_operations_attempted += other.walking_operations_attempted;
        self.bike_operations_attempted += other.bike_operations_attempted;
        self.successors_generated += other.successors_generated;
    }
}

impl ExpansionOutcome {
    fn append(&mut self, other: Self) {
        self.stats.merge(other.stats);
        self.failures.merge(other.failures);
        match (&mut self.successors, other.successors) {
            (Ok(next), Ok(successors)) => next.extend(successors),
            (Ok(_), Err(error)) => self.successors = Err(error),
            (Err(_), _) => {}
        }
    }
}

impl ExpansionInputs<'_> {
    fn check_deadline(&self) -> Result<(), RoutingError> {
        if self.query.deadline.is_elapsed() {
            Err(RoutingError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }

    pub(super) fn resolve_transit(
        &self,
        state: &EvaluationState,
        edge: &CorridorEdge,
        anchor: usize,
    ) -> ExpansionOutcome {
        let mut stats = ExpansionStats::default();
        let successors = (|| {
            let PlannedMovement::Transit(leg) = &edge.planned_movement else {
                return Ok(Vec::new());
            };
            let mut next = Vec::new();
            for equivalent_leg in self.corridor.equivalent_legs_for(self.network, leg) {
                self.check_deadline()?;
                stats.transit_operations_attempted += 1;
                valid_pattern_leg(self.network, self.corridor, leg, &equivalent_leg)?;
                let ready_at = transit_ready_at(state, self.network.same_stop_transfer_seconds());
                if !within_query_bounds(self.query, ready_at) {
                    continue;
                }
                stats.transit_departure_lookups += 1;
                let departures = self.snapshot.departures(
                    &equivalent_leg.pattern_id,
                    equivalent_leg.board_index,
                    ready_at,
                    self.query.limits.max_departures_per_query,
                );
                for trip in departures {
                    self.check_deadline()?;
                    if let Some(successor) = transit_next_state(
                        self.network,
                        self.query,
                        state,
                        trip,
                        &equivalent_leg,
                        anchor,
                        ready_at,
                    ) {
                        stats.successors_generated += 1;
                        next.push(successor);
                    }
                }
            }
            Ok(next)
        })();
        ExpansionOutcome {
            successors,
            stats,
            failures: EvaluationFailures::default(),
        }
    }
}

impl ExpansionResolver<'_> {
    pub async fn expand_edge(
        &self,
        states: &[EvaluationState],
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
    ) -> ExpansionOutcome {
        use futures_util::{StreamExt, stream};
        let concurrency = self
            .inputs
            .query
            .limits
            .max_concurrent_phase2_states
            .clamp(1, 32);
        let mut pending = stream::iter(0..states.len())
            .map(|position| async move {
                (
                    position,
                    self.expand_state(&states[position], corridor, edge, index)
                        .await,
                )
            })
            .buffer_unordered(concurrency);
        let mut observed = std::collections::BTreeMap::new();
        let fatal = loop {
            match self.inputs.query.deadline.run(pending.next()).await {
                Ok(Some((position, outcome))) => {
                    let fatal = outcome.successors.as_ref().err().cloned();
                    observed.insert(position, outcome);
                    if fatal.is_some() {
                        break fatal;
                    }
                }
                Ok(None) => break None,
                Err(error) => break Some(error),
            }
        };
        drop(pending);
        let mut result = ExpansionOutcome {
            successors: Ok(Vec::new()),
            stats: ExpansionStats::default(),
            failures: EvaluationFailures::default(),
        };
        for outcome in observed.into_values() {
            result.append(outcome);
        }
        if let Some(error) = fatal {
            result.successors = Err(error);
        }
        result
    }

    pub async fn expand_state(
        &self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
    ) -> ExpansionOutcome {
        if let Err(error) = self.inputs.check_deadline() {
            return ExpansionOutcome {
                successors: Err(error),
                stats: ExpansionStats::default(),
                failures: EvaluationFailures::default(),
            };
        }
        let mut outcome = self.inputs.resolve_transit(state, edge, index);
        outcome.stats.states_expanded = 1;
        if outcome.successors.is_err() {
            return outcome;
        }
        outcome.append(self.resolve_walking(state, corridor, edge, index).await);
        if outcome.successors.is_err() {
            return outcome;
        }
        outcome.append(self.resolve_bike_share(state, corridor, index).await);
        outcome
    }

    pub(super) async fn resolve_walking(
        &self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
    ) -> ExpansionOutcome {
        let mut stats = ExpansionStats::default();
        let mut failures = EvaluationFailures::default();
        let successors = async {
            self.inputs.check_deadline()?;
            stats.walking_operations_attempted += 1;
            self.inputs.check_deadline()?;
            if let PlannedMovement::RetainedWalking {
                kind,
                duration_seconds,
                distance_metres,
                access_available_at,
            } = &edge.planned_movement
            {
                return Ok(walking_next_state(
                    self.inputs.query,
                    state,
                    ResolvedWalking {
                        kind: *kind,
                        duration_seconds: *duration_seconds,
                        distance_metres: *distance_metres,
                        access_available_at: *access_available_at,
                    },
                    index,
                )
                .into_iter()
                .collect());
            }
            let from = &corridor.anchors()[index];
            let to = &corridor.anchors()[index + 1];
            let result = self.walking.realize(from, to, state.ready_at).await;
            self.inputs.check_deadline()?;
            let (cell, classified) = resolved_data(result);
            failures = classified;
            let Some(MatrixCell::Reachable { duration, distance }) = cell else {
                return Ok(Vec::new());
            };
            let kind = match (&from.kind, &to.kind) {
                (JourneyAnchorKind::Origin, JourneyAnchorKind::Destination) => {
                    WalkingMovementKind::Direct
                }
                (JourneyAnchorKind::Origin, _) => WalkingMovementKind::Access,
                (_, JourneyAnchorKind::Destination) => WalkingMovementKind::Egress,
                _ => WalkingMovementKind::Transfer,
            };
            Ok(walking_next_state(
                self.inputs.query,
                state,
                ResolvedWalking {
                    kind,
                    duration_seconds: duration.0,
                    distance_metres: distance.0,
                    access_available_at: None,
                },
                index,
            )
            .into_iter()
            .collect())
        }
        .await;
        if let Ok(next) = &successors {
            stats.successors_generated = next.len();
        }
        ExpansionOutcome {
            successors,
            stats,
            failures,
        }
    }

    pub(super) async fn resolve_bike_share(
        &self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        index: usize,
    ) -> ExpansionOutcome {
        let mut stats = ExpansionStats::default();
        let mut failures = EvaluationFailures::default();
        let successors = async {
            self.inputs.check_deadline()?;
            let (start_anchor, _) = state.bike_departure(index);
            let request = super::super::bike::BikeMovementRequest {
                from: &corridor.anchors()[start_anchor],
                to: &corridor.anchors()[index + 1],
                from_anchor: start_anchor,
                to_anchor: index + 1,
            };
            let Some(session) = self.bike_share else {
                return Ok(Vec::new());
            };
            let Some(candidate) = session.candidate(&request) else {
                return Ok(Vec::new());
            };
            self.inputs.check_deadline()?;
            stats.bike_operations_attempted += 1;
            let result = session.realize(&request, &candidate).await;
            self.inputs.check_deadline()?;
            let (movements, classified) = resolved_data(result);
            failures = classified;
            Ok(bike_next_states(
                self.inputs.query,
                state,
                movements.unwrap_or_default(),
                index,
            ))
        }
        .await;
        if let Ok(next) = &successors {
            stats.successors_generated = next.len();
        }
        ExpansionOutcome {
            successors,
            stats,
            failures,
        }
    }
}

fn resolved_data<T>(result: Result<T, StreetRoutingError>) -> (Option<T>, EvaluationFailures) {
    let mut failures = EvaluationFailures::default();
    let data = match result {
        Ok(items) => Some(items),
        Err(error) => {
            match error.failure_class() {
                ProviderFailureClass::Transient => failures.transient = true,
                ProviderFailureClass::Internal => failures.internal = true,
                ProviderFailureClass::Unreachable => {}
            }
            None
        }
    };
    (data, failures)
}

pub(super) fn transit_ready_at(state: &EvaluationState, transfer_seconds: u32) -> OffsetDateTime {
    if state.last_was_transit {
        state.ready_at + Duration::seconds(i64::from(transfer_seconds))
    } else {
        state.ready_at
    }
}

pub(super) fn transit_next_state(
    network: &TransitRuntime,
    query: &PlanningQuery,
    state: &EvaluationState,
    trip: &EffectiveTrip,
    leg: &StaticTransitLeg,
    anchor: usize,
    ready_at: OffsetDateTime,
) -> Option<EvaluationState> {
    let arrival = retained_alight_time(network, trip, leg, ready_at)?;
    within_query_bounds(query, arrival).then(|| {
        state.after_movement(JourneyMovement {
            from_anchor: anchor,
            to_anchor: anchor + 1,
            departure_at: trip.stop_times[leg.board_index].departure_at,
            arrival_at: arrival,
            kind: JourneyMovementKind::Transit {
                trip: Arc::new(trip.clone()),
                board_index: leg.board_index,
                alight_index: leg.alight_index,
            },
        })
    })
}

pub(super) struct ResolvedWalking {
    pub kind: WalkingMovementKind,
    pub duration_seconds: u32,
    pub distance_metres: u32,
    pub access_available_at: Option<OffsetDateTime>,
}

pub(super) fn walking_next_state(
    query: &PlanningQuery,
    state: &EvaluationState,
    walking: ResolvedWalking,
    index: usize,
) -> Option<EvaluationState> {
    let arrival = walking
        .access_available_at
        .unwrap_or(state.ready_at + Duration::seconds(i64::from(walking.duration_seconds)));
    within_query_bounds(query, arrival).then(|| {
        state.after_movement(JourneyMovement {
            from_anchor: index,
            to_anchor: index + 1,
            departure_at: state.ready_at,
            arrival_at: arrival,
            kind: JourneyMovementKind::Walking {
                kind: walking.kind,
                duration_seconds: walking.duration_seconds,
                distance_metres: walking.distance_metres,
            },
        })
    })
}

pub(super) fn bike_next_states(
    query: &PlanningQuery,
    state: &EvaluationState,
    movements: Vec<BikeShareMovement>,
    index: usize,
) -> Vec<EvaluationState> {
    let (start_anchor, started_at) = state.bike_departure(index);
    movements
        .into_iter()
        .filter_map(|movement| {
            let arrival = started_at + Duration::seconds(i64::from(movement.total_seconds));
            if !within_query_bounds(query, arrival) {
                return None;
            }
            state.after_bike_share(
                movement,
                start_anchor,
                index + 1,
                started_at,
                arrival,
                query.departure_time,
            )
        })
        .collect()
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
        let walking = WalkingCache::new(street.clone());
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
        assert_eq!((walking.requests(), walking.cache_hits()), (5, 1));
        let other_request = WalkingCache::new(street.clone());
        other_request.realize(&from, &to, departure).await.unwrap();
        assert_eq!(
            (other_request.requests(), other_request.cache_hits()),
            (1, 0)
        );
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
            let walking = WalkingCache::new(street.clone());
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
            assert_eq!((walking.requests(), walking.cache_hits()), (1, 1));
            assert_eq!(street.queries.lock().unwrap().len(), 1);
        }
    }
}
