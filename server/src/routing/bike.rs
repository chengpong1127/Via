use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use time::OffsetDateTime;

use crate::{
    domain::bikeshare::{
        BikeAvailabilitySnapshot, BikeShareNetwork, BikeStationAccess, BikeWalkingAccess,
    },
    domain::ids::BikeStationId,
    domain::location::GeoCoordinate,
    providers::BikeAvailabilitySource,
    street::{
        Coordinate, MatrixCell, MatrixLimits, MatrixOrientation, MatrixQuery, MatrixResult,
        RoutingConstraints, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

use super::request_cache::RequestCache;

use super::{
    corridor::{CorridorAnchor, CorridorAnchorKey},
    realized::{BikeRide, BikeShareMovement},
};

#[derive(Clone, Debug)]
pub struct BikeMovementPolicy {
    pub max_pickup_stations_per_anchor: usize,
    pub max_dropoff_stations_per_anchor: usize,
    pub max_stations_per_endpoint: usize,
    pub endpoint_radius_metres: u32,
    pub max_endpoint_walk_seconds: u32,
    pub max_bike_duration_seconds: u32,
    pub max_bike_distance_metres: u32,
    pub optimistic_walk_speed_metres_per_second: f64,
    pub availability_max_age_seconds: u32,
    pub availability_max_clock_skew_seconds: u32,
}

pub(crate) struct BikeMovementRequest<'a> {
    pub from: &'a CorridorAnchor,
    pub to: &'a CorridorAnchor,
    pub from_anchor: usize,
    pub to_anchor: usize,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct BikeSessionStats {
    pub exact_requests: usize,
    pub cache_hits: usize,
    pub matrix_requests: usize,
}

#[cfg(test)]
#[async_trait::async_trait]
pub(crate) trait TestBikePlanner: Send + Sync {
    fn realization_stats(&self) -> BikeSessionStats {
        BikeSessionStats::default()
    }
    fn has_candidate(&self, request: &BikeMovementRequest<'_>) -> bool;
    async fn realize(
        &self,
        request: &BikeMovementRequest<'_>,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BikePlanningStats {
    pub matrix_requests: usize,
}

#[derive(Clone)]
pub(crate) struct BikeShareMovementPlanner {
    settings: BikeMovementPolicy,
    pub(crate) network: Arc<BikeShareNetwork>,
    pub(crate) availability_source: Arc<dyn BikeAvailabilitySource>,
    pub(crate) street_router: Arc<dyn StreetRouter>,
}

#[derive(Clone)]
pub(crate) struct BikeMovementCandidate {
    pickups: Vec<StationChoice>,
    dropoffs: Vec<StationChoice>,
    pickup_endpoint: Option<Coordinate>,
    dropoff_endpoint: Option<Coordinate>,
}

impl BikeShareMovementPlanner {
    pub(crate) fn new(
        network: Arc<BikeShareNetwork>,
        availability_source: Arc<dyn BikeAvailabilitySource>,
        street_router: Arc<dyn StreetRouter>,
        settings: BikeMovementPolicy,
    ) -> Self {
        Self {
            network,
            availability_source,
            street_router,
            settings,
        }
    }

    pub(crate) fn required_station_ids<'a>(
        &self,
        anchors: impl IntoIterator<Item = &'a CorridorAnchor>,
    ) -> Vec<BikeStationId> {
        let mut ids = HashSet::new();
        let mut seen = HashSet::new();
        for anchor in anchors {
            if !seen.insert(anchor.cache_key()) {
                continue;
            }
            if let Some(id) = anchor.boarding_point() {
                ids.extend(
                    self.network
                        .access_from_boarding_point(id)
                        .iter()
                        .filter(|access| {
                            access.walk_to_station.is_some() || access.walk_from_station.is_some()
                        })
                        .map(|access| access.station_id.clone()),
                );
            } else {
                ids.extend(
                    self.network
                        .stations_within_radius(
                            endpoint_coordinate(anchor),
                            f64::from(self.settings.endpoint_radius_metres),
                        )
                        .into_iter()
                        .map(|station| station.id.clone()),
                );
            }
        }
        let mut ids = ids.into_iter().collect::<Vec<_>>();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    pub(crate) fn movement_candidate(
        &self,
        from: &CorridorAnchor,
        to: &CorridorAnchor,
        snapshot: &BikeAvailabilitySnapshot,
        now: OffsetDateTime,
    ) -> Option<BikeMovementCandidate> {
        let view = self.network.availability_view_with_policy(
            snapshot,
            now,
            time::Duration::seconds(i64::from(self.settings.availability_max_age_seconds)),
            time::Duration::seconds(i64::from(self.settings.availability_max_clock_skew_seconds)),
        );
        let pickups = endpoint_station_choices(&self.network, &view, from, true, &self.settings);
        let dropoffs = endpoint_station_choices(&self.network, &view, to, false, &self.settings);
        if pickups.is_empty() || dropoffs.is_empty() {
            return None;
        }

        if !pickups.iter().any(|pickup| {
            dropoffs
                .iter()
                .any(|dropoff| pickup.station_id != dropoff.station_id)
        }) {
            return None;
        }

        Some(BikeMovementCandidate {
            pickups,
            dropoffs,
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
    ) -> Result<(Vec<BikeShareMovement>, BikePlanningStats), StreetRoutingError> {
        let mut stats = BikePlanningStats::default();
        let bike_movements = self
            .realize_with_stats(candidate, from_anchor, to_anchor, &mut stats)
            .await?;
        Ok((bike_movements, stats))
    }

    async fn realize_with_stats(
        &self,
        candidate: &BikeMovementCandidate,
        from_anchor: usize,
        to_anchor: usize,
        stats: &mut BikePlanningStats,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
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
        let bike_movements = self
            .realize_candidate_matrices(&candidate, from_anchor, to_anchor, limits, stats)
            .await?;
        Ok(finalize_bike_movements(bike_movements))
    }

    async fn realize_candidate_matrices(
        &self,
        candidate: &BikeMovementCandidate,
        from_anchor: usize,
        to_anchor: usize,
        limits: MatrixLimits,
        stats: &mut BikePlanningStats,
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
                        if let Some(bike_movement) = bike_movement_from_cell(
                            (from_anchor, to_anchor),
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
                .map(|choice| station_coordinate(&self.network, &choice.station_id))
                .collect::<Vec<_>>();
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

struct BikeWork<'a> {
    stats: BikePlanningStats,
    total: &'a AtomicUsize,
}

impl Drop for BikeWork<'_> {
    fn drop(&mut self) {
        self.total
            .fetch_add(self.stats.matrix_requests, Ordering::Relaxed);
    }
}

pub(crate) struct BikeShareSession {
    planner: BikeShareMovementPlanner,
    snapshot: Arc<BikeAvailabilitySnapshot>,
    now: OffsetDateTime,
    cache: RequestCache<
        (CorridorAnchorKey, CorridorAnchorKey),
        Result<Vec<BikeShareMovement>, StreetRoutingError>,
    >,
    matrix_requests: AtomicUsize,
    candidates:
        Mutex<HashMap<(CorridorAnchorKey, CorridorAnchorKey), Option<BikeMovementCandidate>>>,
    #[cfg(test)]
    candidate_discoveries: AtomicUsize,
    #[cfg(test)]
    test_planner: Option<Box<dyn TestBikePlanner>>,
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
            cache: RequestCache::default(),
            matrix_requests: AtomicUsize::new(0),
            candidates: Mutex::new(HashMap::new()),
            #[cfg(test)]
            candidate_discoveries: AtomicUsize::new(0),
            #[cfg(test)]
            test_planner: None,
        }
    }

    pub fn realization_stats(&self) -> BikeSessionStats {
        #[cfg(test)]
        if let Some(planner) = &self.test_planner {
            return planner.realization_stats();
        }
        let cache = self.cache.stats();
        BikeSessionStats {
            exact_requests: cache.completed_entries,
            cache_hits: cache.ready_hits + cache.inflight_hits,
            matrix_requests: self.matrix_requests.load(Ordering::Relaxed),
        }
    }
    pub fn candidate(&self, request: &BikeMovementRequest<'_>) -> Option<BikeMovementCandidate> {
        #[cfg(test)]
        if let Some(planner) = &self.test_planner {
            return planner
                .has_candidate(request)
                .then(|| BikeMovementCandidate {
                    pickups: Vec::new(),
                    dropoffs: Vec::new(),
                    pickup_endpoint: None,
                    dropoff_endpoint: None,
                });
        }
        let key = (request.from.cache_key(), request.to.cache_key());
        self.candidates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_insert_with(|| {
                #[cfg(test)]
                {
                    self.candidate_discoveries.fetch_add(1, Ordering::Relaxed);
                }
                self.planner
                    .movement_candidate(request.from, request.to, &self.snapshot, self.now)
            })
            .clone()
    }

    #[cfg(test)]
    pub fn for_test(planner: Box<dyn TestBikePlanner>) -> Self {
        let mut session = Self::new(
            BikeShareMovementPlanner::new(
                Arc::new(BikeShareNetwork::new(Vec::new())),
                Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
                Arc::new(crate::street::FakeStreetRouter::builder().build()),
                crate::config::ViaConfig::default().bike_policy(),
            ),
            Arc::new(BikeAvailabilitySnapshot::new(
                OffsetDateTime::now_utc(),
                HashMap::new(),
            )),
            OffsetDateTime::now_utc(),
        );
        session.test_planner = Some(planner);
        session
    }

    pub async fn realize(
        &self,
        request: &BikeMovementRequest<'_>,
        candidate: &BikeMovementCandidate,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        #[cfg(test)]
        if let Some(planner) = &self.test_planner {
            return planner.realize(request).await;
        }
        let key = (request.from.cache_key(), request.to.cache_key());
        self.cache
            .get_or_init(key, || async {
                let mut work = BikeWork {
                    stats: BikePlanningStats::default(),
                    total: &self.matrix_requests,
                };
                self.planner
                    .realize_with_stats(candidate, 0, 1, &mut work.stats)
                    .await
            })
            .await
            .map(|bike_movements| {
                bike_movements
                    .into_iter()
                    .map(|mut bike_movement| {
                        bike_movement.from_anchor = request.from_anchor;
                        bike_movement.to_anchor = request.to_anchor;
                        bike_movement
                    })
                    .collect()
            })
    }
}

fn walking_access(cell: &MatrixCell, max_walk_seconds: u32) -> Option<BikeWalkingAccess> {
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
            duration_seconds: duration.0,
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

#[derive(Clone)]
struct StationChoice {
    station_id: BikeStationId,
    walking: BikeWalkingAccess,
}

fn station_choices<'a>(
    accesses: impl IntoIterator<Item = &'a BikeStationAccess>,
    pickup: bool,
    settings: &BikeMovementPolicy,
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
        settings.max_pickup_stations_per_anchor
    } else {
        settings.max_dropoff_stations_per_anchor
    });
    choices
}

fn endpoint_coordinate(endpoint: &CorridorAnchor) -> GeoCoordinate {
    GeoCoordinate {
        latitude: endpoint.coordinate.latitude(),
        longitude: endpoint.coordinate.longitude(),
    }
}

fn endpoint_station_choices(
    network: &BikeShareNetwork,
    view: &crate::domain::bikeshare::BikeAvailabilityView<'_>,
    endpoint: &CorridorAnchor,
    pickup: bool,
    settings: &BikeMovementPolicy,
) -> Vec<StationChoice> {
    if let Some(id) = endpoint.boarding_point() {
        return station_choices(
            if pickup {
                view.usable_pickups(id)
            } else {
                view.usable_dropoffs(id)
            },
            pickup,
            settings,
        );
    }
    let point = endpoint_coordinate(endpoint);
    let mut choices = network
        .stations_within_radius(point, f64::from(settings.endpoint_radius_metres))
        .into_iter()
        .filter(|station| {
            if pickup {
                view.can_pick_up_at(&station.id)
            } else {
                view.can_return_at(&station.id)
            }
        })
        .map(|station| {
            let distance = point.distance_metres(station.coordinate);
            StationChoice {
                station_id: station.id.clone(),
                walking: BikeWalkingAccess::new(
                    (distance / settings.optimistic_walk_speed_metres_per_second).floor() as u32,
                    distance.floor() as u32,
                ),
            }
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.walking
            .distance_metres
            .cmp(&right.walking.distance_metres)
            .then(left.station_id.as_str().cmp(right.station_id.as_str()))
    });
    choices.truncate(settings.max_stations_per_endpoint);
    choices
}

fn station_coordinate(network: &BikeShareNetwork, id: &BikeStationId) -> Coordinate {
    let station = network.required_station(id);
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude)
        .expect("validated bike station coordinate")
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
    fn configured_endpoint_radius_controls_snapshot_and_candidate_selection() {
        let now = datetime!(2026-09-21 10:00 UTC);
        let network = Arc::new(BikeShareNetwork::new(vec![
            BikeStation::new(
                BikeStationId::new("first"),
                "First",
                GeoCoordinate {
                    latitude: 25.002,
                    longitude: 121.5,
                },
            ),
            BikeStation::new(
                BikeStationId::new("second"),
                "Second",
                GeoCoordinate {
                    latitude: 25.003,
                    longitude: 121.5,
                },
            ),
        ]));
        let snapshot = BikeAvailabilitySnapshot::new(
            now,
            network
                .stations
                .iter()
                .map(|station| {
                    (
                        station.id.clone(),
                        BikeStationAvailability::new(station.id.clone(), 1, 1, true, now),
                    )
                })
                .collect(),
        );
        let from = CorridorAnchor {
            kind: super::super::realized::JourneyAnchorKind::Origin,
            coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
        };
        let to = CorridorAnchor {
            kind: super::super::realized::JourneyAnchorKind::Destination,
            coordinate: Coordinate::try_new(25.005, 121.5).unwrap(),
        };
        for (radius, expected) in [(100, 0), (300, 2)] {
            let config = crate::config::ViaConfig::from_toml(
                &include_str!("../../config/via.toml").replace(
                    "endpoint_radius_metres = 1500",
                    &format!("endpoint_radius_metres = {radius}"),
                ),
            )
            .unwrap();
            let planner = BikeShareMovementPlanner::new(
                network.clone(),
                Arc::new(UnavailableBikeAvailabilitySource),
                Arc::new(FakeStreetRouter::builder().build()),
                config.bike_policy(),
            );
            assert_eq!(planner.required_station_ids([&from, &to]).len(), expected);
            assert_eq!(
                planner
                    .movement_candidate(&from, &to, &snapshot, now)
                    .is_some(),
                expected > 0
            );
        }
    }

    #[test]
    fn candidate_discovery_rejects_missing_or_same_station_pairs() {
        let now = datetime!(2026-09-21 10:00 UTC);
        let station_id = BikeStationId::new("only-station");
        let network = Arc::new(BikeShareNetwork::new(vec![BikeStation::new(
            station_id.clone(),
            "Only station",
            GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )]));
        let snapshot = BikeAvailabilitySnapshot::new(
            now,
            HashMap::from([(
                station_id.clone(),
                BikeStationAvailability::new(station_id, 1, 1, true, now),
            )]),
        );
        let planner = BikeShareMovementPlanner::new(
            network,
            Arc::new(UnavailableBikeAvailabilitySource),
            Arc::new(FakeStreetRouter::builder().build()),
            crate::config::ViaConfig::default().bike_policy(),
        );
        let from = CorridorAnchor {
            kind: super::super::realized::JourneyAnchorKind::Origin,
            coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
        };
        let near = CorridorAnchor {
            kind: super::super::realized::JourneyAnchorKind::Destination,
            coordinate: Coordinate::try_new(25.001, 121.5).unwrap(),
        };
        let far = CorridorAnchor {
            coordinate: Coordinate::try_new(26.0, 121.5).unwrap(),
            ..near.clone()
        };
        assert!(
            planner
                .movement_candidate(&from, &near, &snapshot, now)
                .is_none()
        );
        assert!(
            planner
                .movement_candidate(&from, &far, &snapshot, now)
                .is_none()
        );
        assert!(
            planner
                .movement_candidate(&far, &near, &snapshot, now)
                .is_none()
        );
    }

    #[test]
    fn required_stations_cover_all_anchors_and_availability_precedes_limits() {
        use super::super::realized::JourneyAnchorKind;
        let origin = Coordinate::try_new(25.0, 121.5).unwrap();
        let destination = Coordinate::try_new(25.1, 121.5).unwrap();
        let mut stations = (0..4)
            .map(|index| {
                BikeStation::new(
                    BikeStationId::new(format!("near-origin-{index}")),
                    "Origin station",
                    GeoCoordinate {
                        latitude: 25.0 + 0.0001 * f64::from(index + 1),
                        longitude: 121.5,
                    },
                )
            })
            .collect::<Vec<_>>();
        stations.push(BikeStation::new(
            BikeStationId::new("near-destination"),
            "Destination station",
            GeoCoordinate {
                latitude: 25.1,
                longitude: 121.5,
            },
        ));
        stations.push(BikeStation::new(
            BikeStationId::new("outside-radius"),
            "Far away",
            GeoCoordinate {
                latitude: 24.0,
                longitude: 121.5,
            },
        ));
        let mut anchors = vec![CorridorAnchor {
            kind: JourneyAnchorKind::Origin,
            coordinate: origin,
        }];
        let mut links = Vec::new();
        for (index, name) in [
            "access",
            "intermediate",
            "transfer-from",
            "transfer-to",
            "egress",
        ]
        .into_iter()
        .enumerate()
        {
            let stop_id = BoardingPointId::new(name);
            let station_id = BikeStationId::new(format!("linked-{name}"));
            stations.push(BikeStation::new(
                station_id.clone(),
                name,
                GeoCoordinate {
                    latitude: 24.0,
                    longitude: 121.5,
                },
            ));
            links.push(BikeStationAccess::new(
                stop_id.clone(),
                station_id,
                (index % 2 == 0).then_some(BikeWalkingAccess::new(10, 20)),
                (index % 2 == 1).then_some(BikeWalkingAccess::new(10, 20)),
            ));
            anchors.push(CorridorAnchor {
                kind: JourneyAnchorKind::BoardingPoint { id: stop_id },
                coordinate: origin,
            });
        }
        anchors.push(CorridorAnchor {
            kind: JourneyAnchorKind::Destination,
            coordinate: destination,
        });
        for index in 0..4 {
            links.push(BikeStationAccess::new(
                BoardingPointId::new("access"),
                BikeStationId::new(format!("near-origin-{index}")),
                Some(BikeWalkingAccess::new(20 + index, 40 + index)),
                None,
            ));
        }
        let network = Arc::new(BikeShareNetwork::new(stations).with_access_links(links));
        let planner = BikeShareMovementPlanner::new(
            network,
            Arc::new(UnavailableBikeAvailabilitySource),
            Arc::new(FakeStreetRouter::builder().build()),
            crate::config::ViaConfig::default().bike_policy(),
        );
        let required = planner.required_station_ids(anchors.iter().chain(anchors.iter()));
        assert_eq!(required.len(), 10);
        assert!(!required.contains(&BikeStationId::new("outside-radius")));
        for name in [
            "access",
            "intermediate",
            "transfer-from",
            "transfer-to",
            "egress",
        ] {
            assert!(required.contains(&BikeStationId::new(format!("linked-{name}"))));
        }
        let now = datetime!(2026-09-21 10:00 UTC);
        let mut snapshot = BikeAvailabilitySnapshot::new(
            now,
            required
                .into_iter()
                .map(|id| {
                    let bikes = u32::from(id.as_str() == "near-origin-3");
                    (
                        id.clone(),
                        BikeStationAvailability::new(id, bikes, 1, true, now),
                    )
                })
                .collect(),
        );
        let candidate = planner
            .movement_candidate(&anchors[0], anchors.last().unwrap(), &snapshot, now)
            .unwrap();
        assert_eq!(candidate.pickups.len(), 1);
        assert_eq!(candidate.pickups[0].station_id.as_str(), "near-origin-3");
        let boarding_candidate = planner
            .movement_candidate(&anchors[1], anchors.last().unwrap(), &snapshot, now)
            .unwrap();
        assert_eq!(boarding_candidate.pickups.len(), 1);
        assert_eq!(
            boarding_candidate.pickups[0].station_id.as_str(),
            "near-origin-3"
        );
        for availability in snapshot.stations.values_mut() {
            availability.available_bikes = 1;
        }
        let candidate = planner
            .movement_candidate(&anchors[0], anchors.last().unwrap(), &snapshot, now)
            .unwrap();
        assert_eq!(
            candidate.pickups.len(),
            crate::config::ViaConfig::default()
                .bikeshare
                .max_stations_per_endpoint
        );
        assert_eq!(candidate.pickups[0].station_id.as_str(), "near-origin-0");
        let boarding_candidate = planner
            .movement_candidate(&anchors[1], anchors.last().unwrap(), &snapshot, now)
            .unwrap();
        assert_eq!(
            boarding_candidate.pickups.len(),
            crate::config::ViaConfig::default()
                .bikeshare
                .max_pickup_stations_per_anchor
        );
    }

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
        let pickups = station_choices(
            network.access_from_boarding_point(&anchor),
            true,
            &BikeMovementPolicy::default(),
        );
        let dropoffs = station_choices(
            network.access_to_boarding_point(&anchor),
            false,
            &BikeMovementPolicy::default(),
        );

        assert_eq!(
            pickups.len(),
            crate::config::ViaConfig::default()
                .bikeshare
                .max_pickup_stations_per_anchor
        );
        assert_eq!(
            dropoffs.len(),
            crate::config::ViaConfig::default()
                .bikeshare
                .max_dropoff_stations_per_anchor
        );
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
            crate::config::ViaConfig::default().bike_policy(),
        );

        let from_endpoint = CorridorAnchor {
            kind: super::super::realized::JourneyAnchorKind::BoardingPoint { id: from.clone() },
            coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
        };
        let to_endpoint = CorridorAnchor {
            kind: super::super::realized::JourneyAnchorKind::BoardingPoint { id: to.clone() },
            coordinate: Coordinate::try_new(25.001, 121.501).unwrap(),
        };
        let candidate = planner
            .movement_candidate(&from_endpoint, &to_endpoint, &snapshot, received_at)
            .unwrap();
        assert_eq!(candidate.pickups.len(), 1);
        assert_eq!(candidate.dropoffs.len(), 1);
        assert_ne!(
            candidate.pickups[0].station_id,
            candidate.dropoffs[0].station_id
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
        use super::super::realized::JourneyAnchorKind;
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
            crate::config::ViaConfig::default().bike_policy(),
        );
        let from = CorridorAnchor {
            kind: JourneyAnchorKind::Origin,
            coordinate: origin,
        };
        let to = CorridorAnchor {
            kind: JourneyAnchorKind::BoardingPoint { id: to_id },
            coordinate: dropoff,
        };
        let session = BikeShareSession::new(planner.clone(), Arc::new(snapshot.clone()), now);
        let request = BikeMovementRequest {
            from: &from,
            to: &to,
            from_anchor: 3,
            to_anchor: 4,
        };
        let candidate = session.candidate(&request).unwrap();
        assert!(session.candidate(&request).is_some());
        assert_eq!(session.candidate_discoveries.load(Ordering::Relaxed), 1);
        let movements = session.realize(&request, &candidate).await.unwrap();
        let bike_movement = &movements[0];
        assert_eq!(bike_movement.total_seconds, 130);
        assert_eq!((bike_movement.from_anchor, bike_movement.to_anchor), (3, 4));
        assert_eq!(session.realization_stats().matrix_requests, 2);
        let remapped = session
            .realize(
                &BikeMovementRequest {
                    from_anchor: 0,
                    to_anchor: 1,
                    ..request
                },
                &candidate,
            )
            .await
            .unwrap();
        let bike_movement = &remapped[0];
        assert_eq!((bike_movement.from_anchor, bike_movement.to_anchor), (0, 1));
        assert_eq!(session.realization_stats().cache_hits, 1);
        assert_eq!(session.realization_stats().matrix_requests, 2);
        assert_eq!(session.candidate_discoveries.load(Ordering::Relaxed), 1);
        let unavailable = BikeShareSession::new(
            planner,
            Arc::new(BikeAvailabilitySnapshot::new(now, HashMap::new())),
            now,
        );
        assert!(unavailable.candidate(&request).is_none());
    }

    #[test]
    fn station_walks_over_fifteen_minutes_are_unavailable() {
        use crate::street::{Metres, Seconds};
        assert!(
            walking_access(
                &MatrixCell::Reachable {
                    duration: Seconds(901),
                    distance: Metres(1)
                },
                900
            )
            .is_none()
        );
        assert!(
            walking_access(
                &MatrixCell::Reachable {
                    duration: Seconds(900),
                    distance: Metres(1)
                },
                900
            )
            .is_some()
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_candidate_discovery_caches_none_once() {
        let now = datetime!(2026-09-21 10:00 UTC);
        let session = Arc::new(BikeShareSession::new(
            BikeShareMovementPlanner::new(
                Arc::new(BikeShareNetwork::new(Vec::new())),
                Arc::new(UnavailableBikeAvailabilitySource),
                Arc::new(FakeStreetRouter::builder().build()),
                crate::config::ViaConfig::default().bike_policy(),
            ),
            Arc::new(BikeAvailabilitySnapshot::new(now, HashMap::new())),
            now,
        ));
        let barrier = Arc::new(tokio::sync::Barrier::new(10));
        let tasks = (0..10)
            .map(|_| {
                let session = session.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    let corridor = crate::routing::corridor::EvaluationCorridor::direct_walking(
                        Coordinate::try_new(25.0, 121.5).unwrap(),
                        Coordinate::try_new(25.01, 121.5).unwrap(),
                    );
                    barrier.wait().await;
                    let request = BikeMovementRequest {
                        from: &corridor.anchors()[0],
                        to: &corridor.anchors()[1],
                        from_anchor: 0,
                        to_anchor: 1,
                    };
                    assert!(session.candidate(&request).is_none());
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(session.candidate_discoveries.load(Ordering::Relaxed), 1);
        assert_eq!(session.realization_stats().exact_requests, 0);
    }
}
