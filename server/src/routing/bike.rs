use std::{collections::HashMap, sync::Arc};
use time::OffsetDateTime;

use crate::{
    domain::bikeshare::{
        BikeAvailabilitySnapshot, BikeShareNetwork, BikeStationAccess, BikeWalkingAccess,
        DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
    },
    domain::ids::BikeStationId,
    domain::location::GeoCoordinate,
    providers::BikeAvailabilitySource,
    street::{
        Coordinate, MatrixCell, MatrixLimits, MatrixOrientation, MatrixQuery, MatrixResult,
        RoutingConstraints, StreetRouter, StreetTravelMode,
    },
};

use super::variant::{BikeRide, BikeShortcut};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BikePlanningStats {
    pub relevant_bike_stations: usize,
    pub candidate_movements: usize,
    pub pickup_stations_considered: usize,
    pub dropoff_stations_considered: usize,
    pub station_pairs: usize,
    pub matrix_requests: usize,
    pub realizations_feasible: usize,
}

pub(crate) const MAX_PICKUP_STATIONS_PER_ANCHOR: usize = 3;
pub(crate) const MAX_DROPOFF_STATIONS_PER_ANCHOR: usize = 3;
pub(crate) const OPTIMISTIC_BIKE_SPEED_METRES_PER_SECOND: f64 = 8.0;
const MAX_DIRECT_BIKE_STATIONS_PER_ENDPOINT: usize = 3;
const MAX_DIRECT_BIKE_WALK_SECONDS: u32 = 15 * 60;
const MAX_BIKE_DURATION_SECONDS: u32 = 60 * 60;
const MAX_BIKE_DISTANCE_METRES: u32 = 20_000;

#[derive(Clone)]
pub(crate) struct BikeSegmentPlanner {
    pub(crate) network: Arc<BikeShareNetwork>,
    pub(crate) availability_source: Arc<dyn BikeAvailabilitySource>,
    pub(crate) street_router: Arc<dyn StreetRouter>,
}

#[derive(Clone)]
pub(crate) struct BikeMovementCandidate {
    pickups: Vec<StationChoice>,
    dropoffs: Vec<StationChoice>,
    pub optimistic_lower_bound_seconds: u32,
}

impl BikeSegmentPlanner {
    pub(crate) fn new(
        network: Arc<BikeShareNetwork>,
        availability_source: Arc<dyn BikeAvailabilitySource>,
        street_router: Arc<dyn StreetRouter>,
    ) -> Self {
        Self {
            network,
            availability_source,
            street_router,
        }
    }

    pub(crate) fn movement_candidate(
        &self,
        from: &crate::domain::ids::BoardingPointId,
        to: &crate::domain::ids::BoardingPointId,
        snapshot: &BikeAvailabilitySnapshot,
        now: OffsetDateTime,
    ) -> Option<BikeMovementCandidate> {
        let view =
            self.network
                .availability_view_at(snapshot, now, DEFAULT_BIKE_AVAILABILITY_MAX_AGE);
        let pickups = station_choices(&self.network, view.usable_pickups(from), true);
        let dropoffs = station_choices(&self.network, view.usable_dropoffs(to), false);
        if pickups.is_empty() || dropoffs.is_empty() {
            return None;
        }

        let optimistic_lower_bound_seconds = pickups
            .iter()
            .flat_map(|pickup| {
                dropoffs.iter().filter_map(move |dropoff| {
                    if pickup.station_id == dropoff.station_id {
                        return None;
                    }
                    let pickup_station = self.network.station(&pickup.station_id)?;
                    let dropoff_station = self.network.station(&dropoff.station_id)?;
                    let bicycle_lower_bound = pickup_station
                        .coordinate
                        .haversine_distance_metres(dropoff_station.coordinate)
                        / OPTIMISTIC_BIKE_SPEED_METRES_PER_SECOND;
                    Some(
                        f64::from(pickup.walking.duration_seconds)
                            + bicycle_lower_bound
                            + f64::from(dropoff.walking.duration_seconds),
                    )
                })
            })
            .reduce(f64::min)?
            .floor()
            .clamp(0.0, f64::from(u32::MAX)) as u32;

        Some(BikeMovementCandidate {
            pickups,
            dropoffs,
            optimistic_lower_bound_seconds,
        })
    }

    pub(crate) async fn realize_candidate(
        &self,
        candidate: &BikeMovementCandidate,
        from_anchor: usize,
        to_anchor: usize,
    ) -> (Vec<BikeShortcut>, BikePlanningStats) {
        let mut stats = corridor_candidate_stats(candidate);
        let limits = self.street_router.matrix_limits();
        if limits.max_sources == 0 || limits.max_targets == 0 {
            return (Vec::new(), stats);
        }

        let shortcuts = self
            .realize_candidate_matrices(candidate, from_anchor, to_anchor, limits, &mut stats)
            .await;
        let shortcuts = finalize_shortcuts(shortcuts, &mut stats);
        (shortcuts, stats)
    }

    async fn realize_candidate_matrices(
        &self,
        candidate: &BikeMovementCandidate,
        from_anchor: usize,
        to_anchor: usize,
        limits: MatrixLimits,
        stats: &mut BikePlanningStats,
    ) -> Vec<BikeShortcut> {
        let mut shortcuts = Vec::new();
        for pickup_chunk in candidate.pickups.chunks(limits.max_sources) {
            for dropoff_chunk in candidate.dropoffs.chunks(limits.max_targets) {
                let Some(sources) = pickup_chunk
                    .iter()
                    .map(|choice| station_coordinate(&self.network, &choice.station_id))
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let Some(targets) = dropoff_chunk
                    .iter()
                    .map(|choice| station_coordinate(&self.network, &choice.station_id))
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                stats.matrix_requests += 1;
                let Ok(result) = self
                    .street_router
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::ManyToMany,
                        travel_mode: StreetTravelMode::Bicycle,
                        sources,
                        targets,
                        constraints: RoutingConstraints::default(),
                    })
                    .await
                else {
                    continue;
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
                        if let Some(shortcut) = bike_shortcut_from_cell(
                            from_anchor,
                            to_anchor,
                            &pickup_chunk[pickup_index].station_id,
                            &dropoff_chunk[dropoff_index].station_id,
                            pickup_chunk[pickup_index].walking,
                            cell,
                            dropoff_chunk[dropoff_index].walking,
                        ) {
                            shortcuts.push(shortcut);
                        }
                    }
                }
            }
        }
        shortcuts
    }

    pub(crate) async fn direct_realizations(
        &self,
        origin: Coordinate,
        destination: Coordinate,
        snapshot: &BikeAvailabilitySnapshot,
        now: OffsetDateTime,
    ) -> (Vec<BikeShortcut>, BikePlanningStats) {
        let view =
            self.network
                .availability_view_at(snapshot, now, DEFAULT_BIKE_AVAILABILITY_MAX_AGE);
        let origin_point = GeoCoordinate {
            latitude: origin.latitude(),
            longitude: origin.longitude(),
        };
        let destination_point = GeoCoordinate {
            latitude: destination.latitude(),
            longitude: destination.longitude(),
        };
        let limits = self.street_router.matrix_limits();
        let station_limit = MAX_DIRECT_BIKE_STATIONS_PER_ENDPOINT
            .min(limits.max_sources)
            .min(limits.max_targets);
        if station_limit == 0 {
            return (Vec::new(), BikePlanningStats::default());
        }
        let (pickups, dropoffs) = select_direct_station_candidates(
            &self.network,
            &view,
            origin_point,
            destination_point,
            station_limit,
        );
        let mut stats = BikePlanningStats {
            relevant_bike_stations: pickups.len() + dropoffs.len(),
            candidate_movements: usize::from(!pickups.is_empty() && !dropoffs.is_empty()),
            pickup_stations_considered: pickups.len(),
            dropoff_stations_considered: dropoffs.len(),
            station_pairs: pickups.len().saturating_mul(dropoffs.len()),
            ..BikePlanningStats::default()
        };
        if pickups.is_empty() || dropoffs.is_empty() {
            return (Vec::new(), stats);
        }

        let Some((walk_to_pickups, walk_from_dropoffs)) = self
            .load_direct_walking_access(origin, destination, &pickups, &dropoffs, &mut stats)
            .await
        else {
            return (Vec::new(), stats);
        };
        if !walk_to_pickups.iter().any(Option::is_some)
            || !walk_from_dropoffs.iter().any(Option::is_some)
        {
            return (Vec::new(), stats);
        }

        let Some(bike_matrix) = self
            .load_direct_bike_matrix(&pickups, &dropoffs, &mut stats)
            .await
        else {
            return (Vec::new(), stats);
        };
        if !matrix_has_shape(&bike_matrix, pickups.len(), dropoffs.len()) {
            return (Vec::new(), stats);
        }

        let shortcuts = build_direct_shortcuts(
            &bike_matrix,
            &pickups,
            &dropoffs,
            &walk_to_pickups,
            &walk_from_dropoffs,
        );
        let shortcuts = finalize_shortcuts(shortcuts, &mut stats);
        (shortcuts, stats)
    }

    async fn load_direct_walking_access(
        &self,
        origin: Coordinate,
        destination: Coordinate,
        pickups: &[(BikeStationId, Coordinate)],
        dropoffs: &[(BikeStationId, Coordinate)],
        stats: &mut BikePlanningStats,
    ) -> Option<(
        Vec<Option<BikeWalkingAccess>>,
        Vec<Option<BikeWalkingAccess>>,
    )> {
        stats.matrix_requests += 1;
        let pickup_walks = self
            .street_router
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![origin],
                targets: pickups.iter().map(|(_, coordinate)| *coordinate).collect(),
                constraints: RoutingConstraints::default(),
            })
            .await
            .ok()?;
        stats.matrix_requests += 1;
        let dropoff_walks = self
            .street_router
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToOne,
                travel_mode: StreetTravelMode::Walking,
                sources: dropoffs.iter().map(|(_, coordinate)| *coordinate).collect(),
                targets: vec![destination],
                constraints: RoutingConstraints::default(),
            })
            .await
            .ok()?;
        if !matrix_has_shape(&pickup_walks, 1, pickups.len())
            || !matrix_has_shape(&dropoff_walks, dropoffs.len(), 1)
        {
            return None;
        }
        let walk_to_pickups = pickup_walks.cells[0]
            .iter()
            .map(|cell| walking_access(cell))
            .collect::<Vec<_>>();
        let walk_from_dropoffs = dropoff_walks
            .cells
            .iter()
            .map(|row| walking_access(&row[0]))
            .collect::<Vec<_>>();
        Some((walk_to_pickups, walk_from_dropoffs))
    }

    async fn load_direct_bike_matrix(
        &self,
        pickups: &[(BikeStationId, Coordinate)],
        dropoffs: &[(BikeStationId, Coordinate)],
        stats: &mut BikePlanningStats,
    ) -> Option<MatrixResult> {
        stats.matrix_requests += 1;
        self.street_router
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToMany,
                travel_mode: StreetTravelMode::Bicycle,
                sources: pickups.iter().map(|(_, coordinate)| *coordinate).collect(),
                targets: dropoffs.iter().map(|(_, coordinate)| *coordinate).collect(),
                constraints: RoutingConstraints::default(),
            })
            .await
            .ok()
    }
}

fn corridor_candidate_stats(candidate: &BikeMovementCandidate) -> BikePlanningStats {
    BikePlanningStats {
        relevant_bike_stations: candidate.pickups.len() + candidate.dropoffs.len(),
        candidate_movements: 1,
        pickup_stations_considered: candidate.pickups.len(),
        dropoff_stations_considered: candidate.dropoffs.len(),
        station_pairs: candidate
            .pickups
            .iter()
            .flat_map(|pickup| {
                candidate
                    .dropoffs
                    .iter()
                    .filter(move |dropoff| pickup.station_id != dropoff.station_id)
            })
            .count(),
        ..BikePlanningStats::default()
    }
}

fn select_direct_station_candidates(
    network: &BikeShareNetwork,
    view: &crate::domain::bikeshare::BikeAvailabilityView<'_>,
    origin: GeoCoordinate,
    destination: GeoCoordinate,
    station_limit: usize,
) -> (
    Vec<(BikeStationId, Coordinate)>,
    Vec<(BikeStationId, Coordinate)>,
) {
    let mut pickups = direct_station_choices(network, view, origin, true);
    let mut dropoffs = direct_station_choices(network, view, destination, false);
    pickups.truncate(station_limit);
    dropoffs.truncate(station_limit);
    (pickups, dropoffs)
}

fn walking_access(cell: &MatrixCell) -> Option<BikeWalkingAccess> {
    match cell {
        MatrixCell::Reachable { duration, distance }
            if duration.0 <= MAX_DIRECT_BIKE_WALK_SECONDS =>
        {
            Some(BikeWalkingAccess::new(duration.0, distance.0))
        }
        _ => None,
    }
}

fn matrix_has_shape(matrix: &MatrixResult, expected_rows: usize, expected_columns: usize) -> bool {
    matrix.cells.len() == expected_rows
        && matrix.cells.iter().all(|row| row.len() == expected_columns)
}

fn bike_shortcut_from_cell(
    from_anchor: usize,
    to_anchor: usize,
    pickup_station_id: &BikeStationId,
    dropoff_station_id: &BikeStationId,
    walk_to_pickup: BikeWalkingAccess,
    cell: &MatrixCell,
    walk_from_dropoff: BikeWalkingAccess,
) -> Option<BikeShortcut> {
    let MatrixCell::Reachable { duration, distance } = cell else {
        return None;
    };
    if duration.0 > MAX_BIKE_DURATION_SECONDS || distance.0 > MAX_BIKE_DISTANCE_METRES {
        return None;
    }
    BikeShortcut::new(
        from_anchor,
        to_anchor,
        pickup_station_id.clone(),
        dropoff_station_id.clone(),
        walk_to_pickup,
        BikeRide {
            duration_seconds: duration.0,
            distance_metres: distance.0,
        },
        walk_from_dropoff,
    )
}

fn finalize_shortcuts(
    mut shortcuts: Vec<BikeShortcut>,
    stats: &mut BikePlanningStats,
) -> Vec<BikeShortcut> {
    shortcuts.sort_by(|left, right| {
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
    shortcuts.dedup();
    stats.realizations_feasible = shortcuts.len();
    shortcuts
}

fn build_direct_shortcuts(
    bike_matrix: &MatrixResult,
    pickups: &[(BikeStationId, Coordinate)],
    dropoffs: &[(BikeStationId, Coordinate)],
    walk_to_pickups: &[Option<BikeWalkingAccess>],
    walk_from_dropoffs: &[Option<BikeWalkingAccess>],
) -> Vec<BikeShortcut> {
    let mut shortcuts = Vec::new();
    for (pickup_index, row) in bike_matrix.cells.iter().enumerate() {
        let Some(walk_to_pickup) = walk_to_pickups[pickup_index] else {
            continue;
        };
        for (dropoff_index, cell) in row.iter().enumerate() {
            let Some(walk_from_dropoff) = walk_from_dropoffs[dropoff_index] else {
                continue;
            };
            if let Some(shortcut) = bike_shortcut_from_cell(
                0,
                1,
                &pickups[pickup_index].0,
                &dropoffs[dropoff_index].0,
                walk_to_pickup,
                cell,
                walk_from_dropoff,
            ) {
                shortcuts.push(shortcut);
            }
        }
    }
    shortcuts
}

#[derive(Clone)]
struct StationChoice {
    station_id: BikeStationId,
    walking: BikeWalkingAccess,
}

fn station_choices<'a>(
    network: &BikeShareNetwork,
    accesses: impl IntoIterator<Item = &'a BikeStationAccess>,
    pickup: bool,
) -> Vec<StationChoice> {
    let mut choices = HashMap::<BikeStationId, BikeWalkingAccess>::new();
    for access in accesses {
        let Some(walking) = (if pickup {
            access.walk_to_station
        } else {
            access.walk_from_station
        }) else {
            continue;
        };
        if network.station(&access.station_id).is_none() {
            continue;
        }
        choices
            .entry(access.station_id.clone())
            .and_modify(|current| {
                if walking.duration_seconds < current.duration_seconds {
                    *current = walking;
                }
            })
            .or_insert(walking);
    }
    let mut choices = choices
        .into_iter()
        .map(|(station_id, walking)| StationChoice {
            station_id,
            walking,
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.walking
            .duration_seconds
            .cmp(&right.walking.duration_seconds)
            .then(
                left.walking
                    .distance_metres
                    .cmp(&right.walking.distance_metres),
            )
            .then(left.station_id.as_str().cmp(right.station_id.as_str()))
    });
    choices.truncate(if pickup {
        MAX_PICKUP_STATIONS_PER_ANCHOR
    } else {
        MAX_DROPOFF_STATIONS_PER_ANCHOR
    });
    choices
}

fn direct_station_choices(
    network: &BikeShareNetwork,
    view: &crate::domain::bikeshare::BikeAvailabilityView<'_>,
    point: GeoCoordinate,
    pickup: bool,
) -> Vec<(BikeStationId, Coordinate)> {
    let mut choices = network
        .stations
        .iter()
        .filter(|station| {
            if pickup {
                view.can_pick_up_at(&station.id)
            } else {
                view.can_return_at(&station.id)
            }
        })
        .filter_map(|station| {
            let coordinate = station_coordinate(network, &station.id)?;
            let distance = point.haversine_distance_metres(station.coordinate);
            (distance <= 1_500.0).then(|| (station.id.clone(), coordinate, distance))
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.2
            .total_cmp(&right.2)
            .then(left.0.as_str().cmp(right.0.as_str()))
    });
    choices.truncate(MAX_DIRECT_BIKE_STATIONS_PER_ENDPOINT);
    choices
        .into_iter()
        .map(|(id, coordinate, _)| (id, coordinate))
        .collect()
}

fn station_coordinate(network: &BikeShareNetwork, id: &BikeStationId) -> Option<Coordinate> {
    let station = network.station(id)?;
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude).ok()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use crate::{
        api::GeoCoordinate,
        domain::{
            bikeshare::{
                BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
                BikeStationAvailability,
            },
            ids::{BikeStationId, BoardingPointId},
        },
        providers::UnavailableBikeAvailabilitySource,
        street::FakeStreetRouter,
    };

    use super::*;

    #[test]
    fn station_choices_are_bounded_to_three_nearest_per_anchor() {
        let anchor = BoardingPointId::new("anchor");
        let stations = (0..5)
            .map(|index| {
                BikeStation::new(
                    BikeStationId::new(format!("station-{index}")),
                    format!("Station {index}"),
                    GeoCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                )
            })
            .collect::<Vec<_>>();
        let access_links = (0..5)
            .map(|index| {
                BikeStationAccess::new(
                    anchor.clone(),
                    BikeStationId::new(format!("station-{index}")),
                    Some(BikeWalkingAccess::new(index + 1, index + 1)),
                    Some(BikeWalkingAccess::new(index + 1, index + 1)),
                )
            })
            .collect();
        let network = BikeShareNetwork::new(stations).with_access_links(access_links);
        let pickups = station_choices(&network, network.access_from_boarding_point(&anchor), true);
        let dropoffs = station_choices(&network, network.access_to_boarding_point(&anchor), false);

        assert_eq!(pickups.len(), MAX_PICKUP_STATIONS_PER_ANCHOR);
        assert_eq!(dropoffs.len(), MAX_DROPOFF_STATIONS_PER_ANCHOR);
        assert_eq!(
            pickups
                .iter()
                .map(|choice| choice.station_id.as_str())
                .collect::<Vec<_>>(),
            vec!["station-0", "station-1", "station-2"]
        );
    }

    #[tokio::test]
    async fn cached_bike_availability_is_checked_at_the_request_time() {
        let from = BoardingPointId::new("from");
        let to = BoardingPointId::new("to");
        let pickup_id = BikeStationId::new("tdx:Taipei:pickup");
        let dropoff_id = BikeStationId::new("tdx:Taipei:dropoff");
        let bike_network = Arc::new(
            BikeShareNetwork::new(vec![
                BikeStation::new(
                    pickup_id.clone(),
                    "Pickup",
                    GeoCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                ),
                BikeStation::new(
                    dropoff_id.clone(),
                    "Dropoff",
                    GeoCoordinate {
                        latitude: 25.001,
                        longitude: 121.501,
                    },
                ),
            ])
            .with_access_links(vec![
                BikeStationAccess::new(
                    from.clone(),
                    pickup_id.clone(),
                    Some(BikeWalkingAccess::new(10, 50)),
                    None,
                ),
                BikeStationAccess::new(
                    to.clone(),
                    dropoff_id.clone(),
                    None,
                    Some(BikeWalkingAccess::new(10, 50)),
                ),
            ])
            .with_revision("fixture"),
        );
        let received_at = datetime!(2026-09-21 10:00 UTC);
        let mut stations = HashMap::new();
        for (station_id, bikes, docks) in [(pickup_id, 1, 0), (dropoff_id, 0, 1)] {
            stations.insert(
                station_id.clone(),
                BikeStationAvailability::new(
                    station_id,
                    bikes,
                    docks,
                    true,
                    received_at - time::Duration::seconds(110),
                ),
            );
        }
        let snapshot = BikeAvailabilitySnapshot::new(received_at, stations);
        let planner = BikeSegmentPlanner::new(
            bike_network,
            Arc::new(UnavailableBikeAvailabilitySource),
            Arc::new(FakeStreetRouter::builder().build()),
        );

        let candidate = planner
            .movement_candidate(&from, &to, &snapshot, received_at)
            .unwrap();
        let station_distance = GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        }
        .haversine_distance_metres(GeoCoordinate {
            latitude: 25.001,
            longitude: 121.501,
        });
        assert_eq!(
            candidate.optimistic_lower_bound_seconds,
            (20.0 + station_distance / OPTIMISTIC_BIKE_SPEED_METRES_PER_SECOND).floor() as u32
        );
        let (_, exact_stats) = planner.realize_candidate(&candidate, 0, 1).await;
        assert_eq!(exact_stats.matrix_requests, 1);
        assert!(
            planner
                .movement_candidate(
                    &from,
                    &to,
                    &snapshot,
                    received_at + time::Duration::seconds(11),
                )
                .is_none()
        );
    }
}
