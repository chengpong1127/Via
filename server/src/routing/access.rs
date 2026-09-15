//! Algorithm-neutral endpoint preparation.
//!
//! This module deliberately stops at street reachability. It does not inspect
//! services, trips, realtime values, or transit topology.

use time::OffsetDateTime;

use crate::{
    api::RequestCoordinate,
    domain::transit::{BikeShareStation, TransitNetwork},
    street::{
        Coordinate, MatrixCell, MatrixOrientation, MatrixQuery, RoutingConstraints, StreetRouter,
        StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    algorithm::{AccessPoint, EgressPoint},
    service::RoutingLimits,
    transfers::TransferPath,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreetMetric {
    pub duration_seconds: u32,
    pub distance_metres: u32,
}

pub(crate) async fn discover_access_points(
    street: &dyn StreetRouter,
    network: &TransitNetwork,
    origin: RequestCoordinate,
    departure_time: OffsetDateTime,
    constraints: RoutingConstraints,
    limits: &RoutingLimits,
) -> Result<Vec<AccessPoint>, StreetRoutingError> {
    let origin = Coordinate::try_new(origin.latitude, origin.longitude)?;
    let stops = nearby_stops(network, origin, limits.max_transit_access_stops, limits);
    if stops.is_empty() {
        return Ok(Vec::new());
    }
    let matrix = street
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            travel_mode: StreetTravelMode::Walking,
            sources: vec![origin],
            targets: stops.iter().map(|(_, coordinate)| *coordinate).collect(),
            constraints,
        })
        .await?;
    let mut result = matrix
        .cells
        .first()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, cell)| {
            let (stop_id, _) = stops.get(index)?;
            let metric = street_metric(cell)?;
            (metric.duration_seconds <= limits.max_walking_seconds).then(|| {
                AccessPoint::new(
                    stop_id.clone(),
                    departure_time,
                    departure_time + time::Duration::seconds(i64::from(metric.duration_seconds)),
                    metric.duration_seconds,
                    metric.distance_metres,
                )
            })
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        left.walking_seconds
            .cmp(&right.walking_seconds)
            .then(left.distance_metres.cmp(&right.distance_metres))
            .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
    });
    result.truncate(limits.max_transit_access_stops.max(1));
    Ok(result)
}

pub(crate) async fn discover_egress_points(
    street: &dyn StreetRouter,
    network: &TransitNetwork,
    destination: RequestCoordinate,
    constraints: RoutingConstraints,
    limits: &RoutingLimits,
) -> Result<Vec<EgressPoint>, StreetRoutingError> {
    let destination = Coordinate::try_new(destination.latitude, destination.longitude)?;
    let stops = nearby_stops(
        network,
        destination,
        limits.max_transit_egress_stops,
        limits,
    );
    if stops.is_empty() {
        return Ok(Vec::new());
    }
    let matrix = street
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::ManyToOne,
            travel_mode: StreetTravelMode::Walking,
            sources: stops.iter().map(|(_, coordinate)| *coordinate).collect(),
            targets: vec![destination],
            constraints,
        })
        .await?;
    let mut result = matrix
        .cells
        .iter()
        .enumerate()
        .filter_map(|(index, cells)| {
            let (stop_id, _) = stops.get(index)?;
            let metric = street_metric(cells.first()?)?;
            (metric.duration_seconds <= limits.max_walking_seconds).then(|| {
                EgressPoint::new(
                    stop_id.clone(),
                    metric.duration_seconds,
                    metric.distance_metres,
                )
            })
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        left.walking_seconds
            .cmp(&right.walking_seconds)
            .then(left.distance_metres.cmp(&right.distance_metres))
            .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
    });
    result.truncate(limits.max_transit_egress_stops.max(1));
    Ok(result)
}

pub(crate) struct BikeEndpointRequest {
    pub origin: RequestCoordinate,
    pub destination: RequestCoordinate,
    pub departure_time: OffsetDateTime,
    pub access_constraints: RoutingConstraints,
    pub egress_constraints: RoutingConstraints,
    pub access: Vec<AccessPoint>,
    pub egress: Vec<EgressPoint>,
}

pub(crate) async fn discover_bike_endpoint_points(
    street: &dyn StreetRouter,
    network: &TransitNetwork,
    request: BikeEndpointRequest,
    limits: &RoutingLimits,
) -> Result<(Vec<AccessPoint>, Vec<EgressPoint>), StreetRoutingError> {
    let BikeEndpointRequest {
        origin,
        destination,
        departure_time,
        access_constraints,
        egress_constraints,
        mut access,
        mut egress,
    } = request;
    let origin = Coordinate::try_new(origin.latitude, origin.longitude)?;
    let destination = Coordinate::try_new(destination.latitude, destination.longitude)?;
    for point in access.clone() {
        let Some(stop) = network.stop(&point.stop_id) else {
            continue;
        };
        let stop = Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)?;
        let pickups = nearby_stations(network, origin, limits.max_bike_pickup_stations, limits);
        let returns = nearby_stations(network, stop, limits.max_bike_return_stations, limits);
        for pickup in pickups {
            let Some(access_metric) = matrix_metric(
                street,
                origin,
                coordinate(pickup)?,
                StreetTravelMode::Walking,
                access_constraints,
            )
            .await?
            else {
                continue;
            };
            for return_station in &returns {
                if pickup.id == return_station.id {
                    continue;
                }
                let Some(bicycle) = matrix_metric(
                    street,
                    coordinate(pickup)?,
                    coordinate(return_station)?,
                    StreetTravelMode::Bicycle,
                    RoutingConstraints {
                        max_distance: Some(crate::street::Metres(limits.max_bike_distance_metres)),
                        ..access_constraints
                    },
                )
                .await?
                else {
                    continue;
                };
                let Some(egress_metric) = matrix_metric(
                    street,
                    coordinate(return_station)?,
                    stop,
                    StreetTravelMode::Walking,
                    access_constraints,
                )
                .await?
                else {
                    continue;
                };
                let total = access_metric
                    .duration_seconds
                    .saturating_add(bicycle.duration_seconds)
                    .saturating_add(egress_metric.duration_seconds);
                if total
                    > point
                        .walking_seconds
                        .saturating_add(limits.max_bike_extra_seconds)
                {
                    continue;
                }
                access.push(AccessPoint::with_path(
                    point.stop_id.clone(),
                    departure_time,
                    TransferPath::walking_and_bike(
                        access_metric.duration_seconds,
                        access_metric.distance_metres,
                        pickup.id.clone(),
                        return_station.id.clone(),
                        bicycle.duration_seconds,
                        bicycle.distance_metres,
                        egress_metric.duration_seconds,
                        egress_metric.distance_metres,
                    ),
                    access_metric
                        .distance_metres
                        .saturating_add(egress_metric.distance_metres),
                ));
            }
        }
    }
    for point in egress.clone() {
        let Some(stop) = network.stop(&point.stop_id) else {
            continue;
        };
        let stop = Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)?;
        let pickups = nearby_stations(network, stop, limits.max_bike_pickup_stations, limits);
        let returns = nearby_stations(
            network,
            destination,
            limits.max_bike_return_stations,
            limits,
        );
        for pickup in pickups {
            let Some(access_metric) = matrix_metric(
                street,
                stop,
                coordinate(pickup)?,
                StreetTravelMode::Walking,
                egress_constraints,
            )
            .await?
            else {
                continue;
            };
            for return_station in &returns {
                if pickup.id == return_station.id {
                    continue;
                }
                let Some(bicycle) = matrix_metric(
                    street,
                    coordinate(pickup)?,
                    coordinate(return_station)?,
                    StreetTravelMode::Bicycle,
                    RoutingConstraints {
                        max_distance: Some(crate::street::Metres(limits.max_bike_distance_metres)),
                        ..egress_constraints
                    },
                )
                .await?
                else {
                    continue;
                };
                let Some(egress_metric) = matrix_metric(
                    street,
                    coordinate(return_station)?,
                    destination,
                    StreetTravelMode::Walking,
                    egress_constraints,
                )
                .await?
                else {
                    continue;
                };
                let total = access_metric
                    .duration_seconds
                    .saturating_add(bicycle.duration_seconds)
                    .saturating_add(egress_metric.duration_seconds);
                if total
                    > point
                        .walking_seconds
                        .saturating_add(limits.max_bike_extra_seconds)
                {
                    continue;
                }
                egress.push(EgressPoint::with_path(
                    point.stop_id.clone(),
                    TransferPath::walking_and_bike(
                        access_metric.duration_seconds,
                        access_metric.distance_metres,
                        pickup.id.clone(),
                        return_station.id.clone(),
                        bicycle.duration_seconds,
                        bicycle.distance_metres,
                        egress_metric.duration_seconds,
                        egress_metric.distance_metres,
                    ),
                    egress_metric.distance_metres,
                ));
            }
        }
    }
    access.sort_by(|left, right| {
        left.available_at
            .cmp(&right.available_at)
            .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
            .then(left.bike_seconds.cmp(&right.bike_seconds))
    });
    egress.sort_by(|left, right| {
        left.walking_seconds
            .saturating_add(left.bike_seconds)
            .cmp(&right.walking_seconds.saturating_add(right.bike_seconds))
            .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
    });
    access.truncate(limits.max_transit_access_stops.max(1));
    egress.truncate(limits.max_transit_egress_stops.max(1));
    Ok((access, egress))
}

pub(crate) async fn discover_direct_bike_paths(
    street: &dyn StreetRouter,
    network: &TransitNetwork,
    origin: RequestCoordinate,
    destination: RequestCoordinate,
    access_constraints: RoutingConstraints,
    egress_constraints: RoutingConstraints,
    limits: &RoutingLimits,
) -> Result<Vec<TransferPath>, StreetRoutingError> {
    let origin = Coordinate::try_new(origin.latitude, origin.longitude)?;
    let destination = Coordinate::try_new(destination.latitude, destination.longitude)?;
    let pickups = nearby_stations(network, origin, limits.max_bike_pickup_stations, limits);
    let returns = nearby_stations(
        network,
        destination,
        limits.max_bike_return_stations,
        limits,
    );
    let mut paths = Vec::new();
    for pickup in pickups {
        let Some(access) = matrix_metric(
            street,
            origin,
            coordinate(pickup)?,
            StreetTravelMode::Walking,
            access_constraints,
        )
        .await?
        else {
            continue;
        };
        for return_station in &returns {
            if pickup.id == return_station.id {
                continue;
            }
            let Some(bicycle) = matrix_metric(
                street,
                coordinate(pickup)?,
                coordinate(return_station)?,
                StreetTravelMode::Bicycle,
                RoutingConstraints {
                    max_distance: Some(crate::street::Metres(limits.max_bike_distance_metres)),
                    ..access_constraints
                },
            )
            .await?
            else {
                continue;
            };
            if bicycle.duration_seconds > limits.max_bike_duration_seconds
                || bicycle.distance_metres > limits.max_bike_distance_metres
            {
                continue;
            }
            let Some(egress) = matrix_metric(
                street,
                coordinate(return_station)?,
                destination,
                StreetTravelMode::Walking,
                egress_constraints,
            )
            .await?
            else {
                continue;
            };
            paths.push(TransferPath::walking_and_bike(
                access.duration_seconds,
                access.distance_metres,
                pickup.id.clone(),
                return_station.id.clone(),
                bicycle.duration_seconds,
                bicycle.distance_metres,
                egress.duration_seconds,
                egress.distance_metres,
            ));
        }
    }
    paths.sort_by(|left, right| {
        left.duration_seconds()
            .cmp(&right.duration_seconds())
            .then(
                left.required_station_ids()[0]
                    .as_str()
                    .cmp(right.required_station_ids()[0].as_str()),
            )
            .then(
                left.required_station_ids()[1]
                    .as_str()
                    .cmp(right.required_station_ids()[1].as_str()),
            )
    });
    paths.dedup();
    paths.truncate(limits.max_bike_connectors.max(1));
    Ok(paths)
}

async fn matrix_metric(
    street: &dyn StreetRouter,
    source: Coordinate,
    target: Coordinate,
    travel_mode: StreetTravelMode,
    constraints: RoutingConstraints,
) -> Result<Option<StreetMetric>, StreetRoutingError> {
    Ok(street
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            travel_mode,
            sources: vec![source],
            targets: vec![target],
            constraints,
        })
        .await?
        .cells
        .first()
        .and_then(|cells| cells.first())
        .and_then(street_metric))
}

fn nearby_stations<'a>(
    network: &'a TransitNetwork,
    coordinate: Coordinate,
    limit: usize,
    limits: &RoutingLimits,
) -> Vec<&'a BikeShareStation> {
    let mut stations = network
        .bike_share_stations
        .iter()
        .filter(|station| {
            geographic_coordinate_distance(
                coordinate,
                Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude)
                    .expect("validated station coordinate"),
            ) <= u64::from(limits.max_bike_search_radius_metres)
        })
        .collect::<Vec<_>>();
    stations.sort_by(|left, right| {
        geographic_coordinate_distance(
            coordinate,
            Coordinate::try_new(left.coordinate.latitude, left.coordinate.longitude)
                .expect("validated station coordinate"),
        )
        .cmp(&geographic_coordinate_distance(
            coordinate,
            Coordinate::try_new(right.coordinate.latitude, right.coordinate.longitude)
                .expect("validated station coordinate"),
        ))
        .then(left.id.as_str().cmp(right.id.as_str()))
    });
    stations.truncate(limit.max(1));
    stations
}

fn coordinate(station: &BikeShareStation) -> Result<Coordinate, StreetRoutingError> {
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude)
}

fn nearby_stops(
    network: &TransitNetwork,
    coordinate: Coordinate,
    limit: usize,
    limits: &RoutingLimits,
) -> Vec<(crate::domain::ids::BoardingPointId, Coordinate)> {
    let request_coordinate = RequestCoordinate {
        latitude: coordinate.latitude(),
        longitude: coordinate.longitude(),
    };
    network
        .nearest_stops(request_coordinate, limit.max(1))
        .into_iter()
        .filter(|stop| {
            geographic_distance_metres(request_coordinate, stop.coordinate)
                <= u64::from(limits.max_walking_radius_metres)
        })
        .filter_map(|stop| {
            Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)
                .ok()
                .map(|coordinate| (stop.id.clone(), coordinate))
        })
        .collect()
}

pub(crate) fn street_metric(cell: &MatrixCell) -> Option<StreetMetric> {
    match cell {
        MatrixCell::Reachable { duration, distance } => Some(StreetMetric {
            duration_seconds: duration.0,
            distance_metres: distance.0,
        }),
        MatrixCell::Unreachable => None,
    }
}

fn geographic_distance_metres(from: RequestCoordinate, to: RequestCoordinate) -> u64 {
    let latitude = ((from.latitude + to.latitude) / 2.0).to_radians();
    let north = (to.latitude - from.latitude) * 111_320.0;
    let east = (to.longitude - from.longitude) * 111_320.0 * latitude.cos();
    north.hypot(east).max(0.0).round() as u64
}

fn geographic_coordinate_distance(from: Coordinate, to: Coordinate) -> u64 {
    let latitude = ((from.latitude() + to.latitude()) / 2.0).to_radians();
    let north = (to.latitude() - from.latitude()) * 111_320.0;
    let east = (to.longitude() - from.longitude()) * 111_320.0 * latitude.cos();
    north.hypot(east).max(0.0).round() as u64
}
