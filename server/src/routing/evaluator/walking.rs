use super::expansion::{ExpansionContext, MovementExpander};
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

pub(crate) struct WalkingExpander {
    street: Arc<dyn StreetRouter>,
    cache: HashMap<
        (CorridorAnchorKey, CorridorAnchorKey, OffsetDateTime),
        Result<MatrixCell, StreetRoutingError>,
    >,
    pub requests: usize,
    pub cache_hits: usize,
}

impl WalkingExpander {
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
                }));
            }
            return Ok(next);
        }
        let from = &context.corridor.anchors()[index];
        let to = &context.corridor.anchors()[index + 1];
        let bound = (from.distance_to(to) / 2.0)
            .floor()
            .clamp(0.0, f64::from(u32::MAX)) as u32;
        if !within_query_bounds(
            context.query,
            state.ready_at + Duration::seconds(i64::from(bound)),
        ) {
            return Ok(next);
        }
        context.charge()?;
        let result = self.realize(from, to, state.ready_at).await;
        let (duration, distance) = match context.finish_realization(result)? {
            Some(MatrixCell::Reachable { duration, distance }) => (duration, distance),
            Some(MatrixCell::Unreachable) | None => return Ok(next),
        };
        let arrival = state.ready_at + Duration::seconds(i64::from(duration.0));
        if !within_query_bounds(context.query, arrival) {
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
        let mut walking = WalkingExpander::new(street.clone());
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
            let mut walking = WalkingExpander::new(street.clone());
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
