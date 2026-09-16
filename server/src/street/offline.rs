//! Deterministic street provider used by offline CLI fixtures.

use async_trait::async_trait;

use crate::domain::location::RequestCoordinate;

use super::{
    Coordinate, Maneuver, MatrixCell, MatrixQuery, MatrixResult, Metres, RouteQuery, Seconds,
    StreetPath, StreetRouter, StreetRoutingError, StreetTravelMode,
};

#[derive(Clone, Copy, Debug)]
pub struct StraightLineStreetRouter {
    walking_speed_metres_per_second: f64,
    bicycle_speed_metres_per_second: f64,
}

impl Default for StraightLineStreetRouter {
    fn default() -> Self {
        Self {
            walking_speed_metres_per_second: 1.4,
            bicycle_speed_metres_per_second: 4.5,
        }
    }
}

impl StraightLineStreetRouter {
    fn metric(
        &self,
        mode: StreetTravelMode,
        origin: Coordinate,
        destination: Coordinate,
    ) -> (Seconds, Metres) {
        let distance = RequestCoordinate {
            latitude: origin.latitude(),
            longitude: origin.longitude(),
        }
        .haversine_distance_metres(RequestCoordinate {
            latitude: destination.latitude(),
            longitude: destination.longitude(),
        })
        .round() as u32;
        let speed = match mode {
            StreetTravelMode::Walking => self.walking_speed_metres_per_second,
            StreetTravelMode::Bicycle => self.bicycle_speed_metres_per_second,
        };
        let duration = ((f64::from(distance) / speed).ceil() as u32).max(1);
        (Seconds(duration), Metres(distance))
    }
}

#[async_trait]
impl StreetRouter for StraightLineStreetRouter {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        query.validate()?;
        let cells = query
            .sources
            .iter()
            .map(|source| {
                query
                    .targets
                    .iter()
                    .map(|target| {
                        let (duration, distance) = self.metric(query.travel_mode, *source, *target);
                        if query
                            .constraints
                            .max_distance
                            .is_some_and(|maximum| distance.0 > maximum.0)
                        {
                            MatrixCell::Unreachable
                        } else {
                            MatrixCell::Reachable { duration, distance }
                        }
                    })
                    .collect()
            })
            .collect();
        Ok(MatrixResult { cells })
    }

    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        let (duration, distance) = self.metric(query.travel_mode, query.origin, query.destination);
        if query
            .constraints
            .max_distance
            .is_some_and(|maximum| distance.0 > maximum.0)
        {
            return Err(StreetRoutingError::Unreachable);
        }
        Ok(StreetPath {
            duration,
            distance,
            geometry: vec![query.origin, query.destination],
            maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
            attribution: Vec::new(),
        })
    }
}
