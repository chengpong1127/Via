use super::*;

impl BikeShareMovementPlanner {
    pub(super) async fn realize(
        &self,
        street: &dyn StreetRouter,
        candidate: &BikeMovementCandidate,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        let limits = street.matrix_limits();
        if limits.max_sources == 0 || limits.max_targets == 0 {
            return Ok(Vec::new());
        }

        let mut candidate = candidate.clone();
        if let Some(endpoint) = candidate.pickup_endpoint {
            candidate.pickups = self
                .exact_station_walks(street, endpoint, &candidate.pickups, true, limits)
                .await?;
            if candidate.pickups.is_empty() {
                return Ok(Vec::new());
            }
        }
        if let Some(endpoint) = candidate.dropoff_endpoint {
            candidate.dropoffs = self
                .exact_station_walks(street, endpoint, &candidate.dropoffs, false, limits)
                .await?;
            if candidate.dropoffs.is_empty() {
                return Ok(Vec::new());
            }
        }
        let bike_movements = self
            .realize_candidate_matrices(street, &candidate, limits)
            .await?;
        Ok(finalize_bike_movements(bike_movements))
    }

    async fn realize_candidate_matrices(
        &self,
        street: &dyn StreetRouter,
        candidate: &BikeMovementCandidate,
        limits: MatrixLimits,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        let mut bike_movements = Vec::new();
        let mut failure = None;
        for pickup_chunk in candidate.pickups.chunks(limits.max_sources) {
            for dropoff_chunk in candidate.dropoffs.chunks(limits.max_targets) {
                let sources = pickup_chunk
                    .iter()
                    .map(|choice| station_coordinate(&self.network, &choice.station_id))
                    .collect::<Vec<_>>();
                let targets = dropoff_chunk
                    .iter()
                    .map(|choice| station_coordinate(&self.network, &choice.station_id))
                    .collect::<Vec<_>>();
                let result = street
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::ManyToMany,
                        travel_mode: StreetTravelMode::Bicycle,
                        sources,
                        targets,
                        constraints: RoutingConstraints::default(),
                    })
                    .await;
                let result = match result {
                    Ok(result) => result,
                    Err(error) => {
                        failure = Some(error);
                        continue;
                    }
                };
                if !matrix_has_shape(&result, pickup_chunk.len(), dropoff_chunk.len()) {
                    continue;
                }
                for (pickup_index, row) in result.cells.iter().enumerate() {
                    for (dropoff_index, cell) in row.iter().enumerate() {
                        if pickup_chunk[pickup_index].station_id
                            == dropoff_chunk[dropoff_index].station_id
                        {
                            continue;
                        }
                        if let Some(bike_movement) = bike_movement_from_cell(
                            (0, 1),
                            &pickup_chunk[pickup_index].station_id,
                            &dropoff_chunk[dropoff_index].station_id,
                            pickup_chunk[pickup_index].walking,
                            cell,
                            dropoff_chunk[dropoff_index].walking,
                            &self.settings,
                        ) {
                            bike_movements.push(bike_movement);
                        }
                    }
                }
            }
        }
        if bike_movements.is_empty()
            && let Some(error) = failure
        {
            return Err(error);
        }
        Ok(bike_movements)
    }

    async fn exact_station_walks(
        &self,
        street: &dyn StreetRouter,
        endpoint: Coordinate,
        choices: &[StationChoice],
        pickup: bool,
        limits: MatrixLimits,
    ) -> Result<Vec<StationChoice>, StreetRoutingError> {
        let mut exact = Vec::new();
        let mut failure = None;
        for chunk in choices.chunks(if pickup {
            limits.max_targets
        } else {
            limits.max_sources
        }) {
            let coordinates = chunk
                .iter()
                .map(|choice| station_coordinate(&self.network, &choice.station_id))
                .collect::<Vec<_>>();
            let result = street
                .matrix(MatrixQuery {
                    orientation: if pickup {
                        MatrixOrientation::OneToMany
                    } else {
                        MatrixOrientation::ManyToOne
                    },
                    travel_mode: StreetTravelMode::Walking,
                    sources: if pickup {
                        vec![endpoint]
                    } else {
                        coordinates.clone()
                    },
                    targets: if pickup { coordinates } else { vec![endpoint] },
                    constraints: RoutingConstraints::default(),
                })
                .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    failure = Some(error);
                    continue;
                }
            };
            if !matrix_has_shape(
                &result,
                if pickup { 1 } else { chunk.len() },
                if pickup { chunk.len() } else { 1 },
            ) {
                continue;
            }
            for (index, choice) in chunk.iter().enumerate() {
                let cell = if pickup {
                    &result.cells[0][index]
                } else {
                    &result.cells[index][0]
                };
                if let Some(walking) = walking_access(cell, self.settings.max_endpoint_walk_seconds)
                {
                    exact.push(StationChoice {
                        station_id: choice.station_id.clone(),
                        walking,
                    });
                }
            }
        }
        if exact.is_empty()
            && let Some(error) = failure
        {
            return Err(error);
        }
        Ok(exact)
    }
}

pub(super) fn walking_access(
    cell: &MatrixCell,
    max_walk_seconds: u32,
) -> Option<BikeWalkingAccess> {
    match cell {
        MatrixCell::Reachable { duration, distance } if duration.0 <= max_walk_seconds => {
            Some(BikeWalkingAccess::new(duration.0, distance.0))
        }
        _ => None,
    }
}

fn matrix_has_shape(matrix: &MatrixResult, expected_rows: usize, expected_columns: usize) -> bool {
    matrix.cells.len() == expected_rows
        && matrix.cells.iter().all(|row| row.len() == expected_columns)
}

fn bike_movement_from_cell(
    anchors: (usize, usize),
    pickup_station_id: &BikeStationId,
    dropoff_station_id: &BikeStationId,
    walk_to_pickup: BikeWalkingAccess,
    cell: &MatrixCell,
    walk_from_dropoff: BikeWalkingAccess,
    settings: &BikeMovementPolicy,
) -> Option<BikeShareMovement> {
    let (from_anchor, to_anchor) = anchors;
    let MatrixCell::Reachable { duration, distance } = cell else {
        return None;
    };
    if duration.0 > settings.max_bike_duration_seconds
        || distance.0 > settings.max_bike_distance_metres
    {
        return None;
    }
    BikeShareMovement::new(
        from_anchor,
        to_anchor,
        pickup_station_id.clone(),
        dropoff_station_id.clone(),
        walk_to_pickup,
        BikeRide {
            duration_seconds: duration.0.saturating_add(settings.rental_handling_seconds),
            distance_metres: distance.0,
        },
        walk_from_dropoff,
    )
}

fn finalize_bike_movements(mut bike_movements: Vec<BikeShareMovement>) -> Vec<BikeShareMovement> {
    bike_movements.sort_by(|left, right| {
        (
            left.total_seconds,
            left.pickup_station_id.as_str(),
            left.dropoff_station_id.as_str(),
        )
            .cmp(&(
                right.total_seconds,
                right.pickup_station_id.as_str(),
                right.dropoff_station_id.as_str(),
            ))
    });
    bike_movements.dedup();
    bike_movements
}

fn station_coordinate(network: &BikeShareNetwork, id: &BikeStationId) -> Coordinate {
    let station = network.required_station(id);
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude)
        .expect("validated bike station coordinate")
}
