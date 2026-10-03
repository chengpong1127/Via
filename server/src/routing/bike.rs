use async_trait::async_trait;
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
        RoutingConstraints, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    surface::{
        EndpointKey, SurfaceEndpoint, SurfaceKind, SurfaceMovement, SurfaceMovementPlanner,
        SurfaceRequest,
    },
    variant::{BikeRide, BikeShortcut},
};

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
const MAX_BIKE_STATIONS_PER_ENDPOINT: usize = 3;
const OPTIMISTIC_WALK_SPEED_METRES_PER_SECOND: f64 = 2.0;
const MAX_BIKE_WALK_SECONDS: u32 = 15 * 60;
const MAX_BIKE_DURATION_SECONDS: u32 = 60 * 60;
const MAX_BIKE_DISTANCE_METRES: u32 = 20_000;

#[derive(Clone)]
pub(crate) struct BikeShareMovementPlanner {
    pub(crate) network: Arc<BikeShareNetwork>,
    pub(crate) availability_source: Arc<dyn BikeAvailabilitySource>,
    pub(crate) street_router: Arc<dyn StreetRouter>,
}

#[derive(Clone)]
pub(crate) struct BikeMovementCandidate {
    pickups: Vec<StationChoice>,
    dropoffs: Vec<StationChoice>,
    pub optimistic_lower_bound_seconds: u32,
    pickup_endpoint: Option<Coordinate>,
    dropoff_endpoint: Option<Coordinate>,
}

impl BikeShareMovementPlanner {
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
        from: &SurfaceEndpoint,
        to: &SurfaceEndpoint,
        snapshot: &BikeAvailabilitySnapshot,
        now: OffsetDateTime,
    ) -> Option<BikeMovementCandidate> {
        let view =
            self.network
                .availability_view_at(snapshot, now, DEFAULT_BIKE_AVAILABILITY_MAX_AGE);
        let pickups = endpoint_station_choices(&self.network, &view, from, true);
        let dropoffs = endpoint_station_choices(&self.network, &view, to, false);
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
            pickup_endpoint: from.boarding_point().is_none().then_some(from.coordinate),
            dropoff_endpoint: to.boarding_point().is_none().then_some(to.coordinate),
        })
    }

    #[cfg(test)]
    pub(crate) async fn realize_candidate(
        &self,
        candidate: &BikeMovementCandidate,
        from_anchor: usize,
        to_anchor: usize,
    ) -> Result<(Vec<BikeShortcut>, BikePlanningStats), StreetRoutingError> {
        let mut stats = corridor_candidate_stats(candidate);
        let shortcuts = self
            .realize_with_stats(candidate, from_anchor, to_anchor, &mut stats)
            .await?;
        Ok((shortcuts, stats))
    }

    async fn realize_with_stats(
        &self,
        candidate: &BikeMovementCandidate,
        from_anchor: usize,
        to_anchor: usize,
        stats: &mut BikePlanningStats,
    ) -> Result<Vec<BikeShortcut>, StreetRoutingError> {
        let limits = self.street_router.matrix_limits();
        if limits.max_sources == 0 || limits.max_targets == 0 {
            return Ok(Vec::new());
        }

        let mut candidate = candidate.clone();
        if let Some(endpoint) = candidate.pickup_endpoint {
            candidate.pickups = self
                .exact_station_walks(endpoint, &candidate.pickups, true, limits, stats)
                .await?;
            if candidate.pickups.is_empty() {
                return Ok(Vec::new());
            }
        }
        if let Some(endpoint) = candidate.dropoff_endpoint {
            candidate.dropoffs = self
                .exact_station_walks(endpoint, &candidate.dropoffs, false, limits, stats)
                .await?;
            if candidate.dropoffs.is_empty() {
                return Ok(Vec::new());
            }
        }
        let shortcuts = self
            .realize_candidate_matrices(&candidate, from_anchor, to_anchor, limits, stats)
            .await?;
        Ok(finalize_shortcuts(shortcuts, stats))
    }

    async fn realize_candidate_matrices(
        &self,
        candidate: &BikeMovementCandidate,
        from_anchor: usize,
        to_anchor: usize,
        limits: MatrixLimits,
        stats: &mut BikePlanningStats,
    ) -> Result<Vec<BikeShortcut>, StreetRoutingError> {
        let mut shortcuts = Vec::new();
        let mut failure = None;
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
                let result = self
                    .street_router
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
        if shortcuts.is_empty()
            && let Some(error) = failure
        {
            return Err(error);
        }
        Ok(shortcuts)
    }

    async fn exact_station_walks(
        &self,
        endpoint: Coordinate,
        choices: &[StationChoice],
        pickup: bool,
        limits: MatrixLimits,
        stats: &mut BikePlanningStats,
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
                .filter_map(|choice| station_coordinate(&self.network, &choice.station_id))
                .collect::<Vec<_>>();
            if coordinates.len() != chunk.len() {
                continue;
            }
            stats.matrix_requests += 1;
            let result = self
                .street_router
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
                if let Some(walking) = walking_access(cell) {
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

pub(crate) struct BikeShareSession {
    planner: BikeShareMovementPlanner,
    snapshot: Arc<BikeAvailabilitySnapshot>,
    now: OffsetDateTime,
    cache: HashMap<(EndpointKey, EndpointKey), Result<Vec<BikeShortcut>, StreetRoutingError>>,
    pub stats: BikePlanningStats,
    pub cache_hits: usize,
}

impl BikeShareSession {
    pub(crate) fn new(
        planner: BikeShareMovementPlanner,
        snapshot: Arc<BikeAvailabilitySnapshot>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            planner,
            snapshot,
            now,
            cache: HashMap::new(),
            stats: BikePlanningStats::default(),
            cache_hits: 0,
        }
    }
}

#[async_trait]
impl SurfaceMovementPlanner for BikeShareSession {
    fn cache_hits(&self) -> usize {
        self.cache_hits
    }
    fn matrix_requests(&self) -> usize {
        self.stats.matrix_requests
    }
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::BikeShare
    }

    fn optimistic_duration(&self, request: &SurfaceRequest<'_>) -> Option<u32> {
        self.planner
            .movement_candidate(request.from, request.to, &self.snapshot, self.now)
            .map(|candidate| candidate.optimistic_lower_bound_seconds)
    }

    async fn realize(
        &mut self,
        request: &SurfaceRequest<'_>,
    ) -> Result<Vec<SurfaceMovement>, StreetRoutingError> {
        let key = (request.from.cache_key(), request.to.cache_key());
        if self.cache.contains_key(&key) {
            self.cache_hits += 1;
        } else {
            let result = if let Some(candidate) =
                self.planner
                    .movement_candidate(request.from, request.to, &self.snapshot, self.now)
            {
                let mut stats = corridor_candidate_stats(&candidate);
                let result = self
                    .planner
                    .realize_with_stats(&candidate, 0, 1, &mut stats)
                    .await;
                self.stats.relevant_bike_stations += stats.relevant_bike_stations;
                self.stats.candidate_movements += stats.candidate_movements;
                self.stats.pickup_stations_considered += stats.pickup_stations_considered;
                self.stats.dropoff_stations_considered += stats.dropoff_stations_considered;
                self.stats.station_pairs += stats.station_pairs;
                self.stats.matrix_requests += stats.matrix_requests;
                self.stats.realizations_feasible += stats.realizations_feasible;
                result
            } else {
                Ok(Vec::new())
            };
            self.cache.insert(key.clone(), result);
        }
        self.cache[&key].clone().map(|shortcuts| {
            shortcuts
                .into_iter()
                .map(|mut shortcut| {
                    shortcut.from_anchor = request.from_anchor;
                    shortcut.to_anchor = request.to_anchor;
                    SurfaceMovement::BikeShare(shortcut)
                })
                .collect()
        })
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

fn walking_access(cell: &MatrixCell) -> Option<BikeWalkingAccess> {
    match cell {
        MatrixCell::Reachable { duration, distance } if duration.0 <= MAX_BIKE_WALK_SECONDS => {
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

fn endpoint_station_choices(
    network: &BikeShareNetwork,
    view: &crate::domain::bikeshare::BikeAvailabilityView<'_>,
    endpoint: &SurfaceEndpoint,
    pickup: bool,
) -> Vec<StationChoice> {
    if let Some(id) = endpoint.boarding_point() {
        return station_choices(
            network,
            if pickup {
                view.usable_pickups(id)
            } else {
                view.usable_dropoffs(id)
            },
            pickup,
        );
    }
    let point = GeoCoordinate {
        latitude: endpoint.coordinate.latitude(),
        longitude: endpoint.coordinate.longitude(),
    };
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
            station_coordinate(network, &station.id)?;
            let distance = point.haversine_distance_metres(station.coordinate);
            (distance <= 1_500.0).then(|| StationChoice {
                station_id: station.id.clone(),
                walking: BikeWalkingAccess::new(
                    (distance / OPTIMISTIC_WALK_SPEED_METRES_PER_SECOND).floor() as u32,
                    distance.floor() as u32,
                ),
            })
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.walking
            .distance_metres
            .cmp(&right.walking.distance_metres)
            .then(left.station_id.as_str().cmp(right.station_id.as_str()))
    });
    choices.truncate(MAX_BIKE_STATIONS_PER_ENDPOINT);
    choices
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
        let planner = BikeShareMovementPlanner::new(
            bike_network,
            Arc::new(UnavailableBikeAvailabilitySource),
            Arc::new(FakeStreetRouter::builder().build()),
        );

        let from_endpoint = SurfaceEndpoint {
            kind: super::super::variant::JourneyAnchorKind::BoardingPoint { id: from.clone() },
            coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
        };
        let to_endpoint = SurfaceEndpoint {
            kind: super::super::variant::JourneyAnchorKind::BoardingPoint { id: to.clone() },
            coordinate: Coordinate::try_new(25.001, 121.501).unwrap(),
        };
        let candidate = planner
            .movement_candidate(&from_endpoint, &to_endpoint, &snapshot, received_at)
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
        let (_, exact_stats) = planner.realize_candidate(&candidate, 0, 1).await.unwrap();
        assert_eq!(exact_stats.matrix_requests, 1);
        assert!(
            planner
                .movement_candidate(
                    &from_endpoint,
                    &to_endpoint,
                    &snapshot,
                    received_at + time::Duration::seconds(11),
                )
                .is_none()
        );
    }
    #[tokio::test]
    async fn generic_endpoints_share_bicycle_realization_and_cache() {
        use super::super::variant::JourneyAnchorKind;
        use crate::street::{Metres, Seconds};
        let now = datetime!(2026-09-21 10:00 UTC);
        let origin = Coordinate::try_new(25.0, 121.5).unwrap();
        let pickup = Coordinate::try_new(25.0001, 121.5).unwrap();
        let dropoff = Coordinate::try_new(25.01, 121.5).unwrap();
        let to_id = BoardingPointId::new("to");
        let pickup_id = BikeStationId::new("pickup");
        let dropoff_id = BikeStationId::new("dropoff");
        let network = Arc::new(
            BikeShareNetwork::new(vec![
                BikeStation::new(
                    pickup_id.clone(),
                    "Pickup",
                    GeoCoordinate {
                        latitude: pickup.latitude(),
                        longitude: pickup.longitude(),
                    },
                ),
                BikeStation::new(
                    dropoff_id.clone(),
                    "Dropoff",
                    GeoCoordinate {
                        latitude: dropoff.latitude(),
                        longitude: dropoff.longitude(),
                    },
                ),
            ])
            .with_access_links(vec![BikeStationAccess::new(
                to_id.clone(),
                dropoff_id.clone(),
                None,
                Some(BikeWalkingAccess::new(20, 30)),
            )]),
        );
        let snapshot = BikeAvailabilitySnapshot::new(
            now,
            HashMap::from([
                (
                    pickup_id.clone(),
                    BikeStationAvailability::new(pickup_id, 1, 0, true, now),
                ),
                (
                    dropoff_id.clone(),
                    BikeStationAvailability::new(dropoff_id, 0, 1, true, now),
                ),
            ]),
        );
        let planner = BikeShareMovementPlanner::new(
            network,
            Arc::new(UnavailableBikeAvailabilitySource),
            Arc::new(
                FakeStreetRouter::builder()
                    .matrix_leg(
                        StreetTravelMode::Walking,
                        origin,
                        pickup,
                        Seconds(10),
                        Metres(12),
                    )
                    .matrix_leg(
                        StreetTravelMode::Bicycle,
                        pickup,
                        dropoff,
                        Seconds(100),
                        Metres(1000),
                    )
                    .build(),
            ),
        );
        let from = SurfaceEndpoint {
            kind: JourneyAnchorKind::Origin,
            coordinate: origin,
        };
        let to = SurfaceEndpoint {
            kind: JourneyAnchorKind::BoardingPoint { id: to_id },
            coordinate: dropoff,
        };
        let mut session = BikeShareSession::new(planner.clone(), Arc::new(snapshot.clone()), now);
        let request = SurfaceRequest {
            from: &from,
            to: &to,
            departure_at: now,
            from_anchor: 3,
            to_anchor: 4,
        };
        assert!(session.optimistic_duration(&request).is_some());
        let movements = session.realize(&request).await.unwrap();
        let SurfaceMovement::BikeShare(shortcut) = &movements[0] else {
            panic!("expected BikeShare");
        };
        assert_eq!(shortcut.total_seconds, 130);
        assert_eq!((shortcut.from_anchor, shortcut.to_anchor), (3, 4));
        assert_eq!(session.stats.matrix_requests, 2);
        let remapped = session
            .realize(&SurfaceRequest {
                from_anchor: 0,
                to_anchor: 1,
                ..request
            })
            .await
            .unwrap();
        let SurfaceMovement::BikeShare(shortcut) = &remapped[0] else {
            panic!("expected BikeShare");
        };
        assert_eq!((shortcut.from_anchor, shortcut.to_anchor), (0, 1));
        assert_eq!(session.cache_hits, 1);
        assert_eq!(session.stats.matrix_requests, 2);
    }

    #[test]
    fn station_walks_over_fifteen_minutes_are_unavailable() {
        use crate::street::{Metres, Seconds};
        assert!(
            walking_access(&MatrixCell::Reachable {
                duration: Seconds(901),
                distance: Metres(1)
            })
            .is_none()
        );
        assert!(
            walking_access(&MatrixCell::Reachable {
                duration: Seconds(900),
                distance: Metres(1)
            })
            .is_some()
        );
    }
}
