//! Algorithm-neutral street access and egress discovery.

use time::OffsetDateTime;

use crate::{
    api::RequestCoordinate,
    domain::transit::TransitNetwork,
    street::{
        Coordinate, MatrixCell, MatrixOrientation, MatrixQuery, RoutingConstraints, StreetRouter,
        StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    algorithm::{AccessPoint, EgressPoint},
    service::RoutingLimits,
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
        .stops_within_radius(request_coordinate, limits.max_walking_radius_metres)
        .into_iter()
        .take(limit.max(1))
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
