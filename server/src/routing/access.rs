//! Algorithm-neutral street access and egress discovery.

use time::OffsetDateTime;

use crate::{
    api::RequestCoordinate,
    domain::transit::{TransitMode, TransitNetwork},
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
    Ok(prune_access_coverage(
        network,
        result,
        limits.max_transit_access_stops,
    ))
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
    Ok(prune_egress_coverage(
        network,
        result,
        limits.max_transit_egress_stops,
    ))
}

fn prune_access_coverage(
    network: &TransitNetwork,
    points: Vec<AccessPoint>,
    limit: usize,
) -> Vec<AccessPoint> {
    let mut retained = Vec::with_capacity(points.len());
    for point in points {
        if retained.iter().any(|existing: &AccessPoint| {
            existing.walking_seconds <= point.walking_seconds
                && stop_coverage_dominates(network, &existing.stop_id, &point.stop_id)
        }) {
            continue;
        }
        retained.push(point);
    }
    retained.truncate(limit.max(1));
    retained
}

fn prune_egress_coverage(
    network: &TransitNetwork,
    points: Vec<EgressPoint>,
    limit: usize,
) -> Vec<EgressPoint> {
    let mut retained = Vec::with_capacity(points.len());
    for point in points {
        if retained.iter().any(|existing: &EgressPoint| {
            existing.walking_seconds <= point.walking_seconds
                && stop_coverage_dominates(network, &existing.stop_id, &point.stop_id)
        }) {
            continue;
        }
        retained.push(point);
    }
    retained.truncate(limit.max(1));
    retained
}

fn stop_coverage_dominates(
    network: &TransitNetwork,
    dominant_stop: &crate::domain::ids::BoardingPointId,
    candidate_stop: &crate::domain::ids::BoardingPointId,
) -> bool {
    network
        .services_at_stop(candidate_stop)
        .iter()
        .all(|service_id| network.services_at_stop(dominant_stop).contains(service_id))
        && network
            .modes_at_stop(candidate_stop)
            .iter()
            .all(|mode| network.modes_at_stop(dominant_stop).contains(mode))
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
    let candidates = network
        .stops_within_radius(request_coordinate, limits.max_walking_radius_metres)
        .into_iter()
        .filter_map(|stop| {
            Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)
                .ok()
                .map(|coordinate| (stop.id.clone(), coordinate))
        })
        .collect::<Vec<_>>();
    let limit = limit.max(1);
    let mut selected = Vec::with_capacity(limit.min(candidates.len()));
    for mode in [
        TransitMode::Bus,
        TransitMode::Metro,
        TransitMode::Rail,
        TransitMode::HighSpeedRail,
    ] {
        if let Some(candidate) = candidates
            .iter()
            .find(|(stop_id, _)| network.modes_at_stop(stop_id).contains(&mode))
            && !selected.iter().any(
                |(stop_id, _): &(crate::domain::ids::BoardingPointId, Coordinate)| {
                    stop_id == &candidate.0
                },
            )
        {
            selected.push(candidate.clone());
        }
    }
    for candidate in candidates {
        if selected.len() >= limit {
            break;
        }
        if !selected.iter().any(|(stop_id, _)| stop_id == &candidate.0) {
            selected.push(candidate);
        }
    }
    selected
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

#[cfg(test)]
mod tests {
    use super::{RoutingLimits, nearby_stops, stop_coverage_dominates};
    use crate::{
        api::RequestCoordinate,
        domain::{
            ids::BoardingPointId,
            transit::{TransitMode, TransitNetwork, TransitService, TransitStop},
        },
        street::Coordinate,
    };

    fn stop(id: &str) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(id),
            id,
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )
    }

    fn stop_at(id: &str, coordinate: RequestCoordinate) -> TransitStop {
        TransitStop::new(BoardingPointId::new(id), id, coordinate)
    }

    #[test]
    fn coverage_dominance_keeps_unique_service_and_mode_stops() {
        let network = TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("a"), stop("b"), stop("c"), stop("d")],
            vec![
                TransitService::new(
                    crate::domain::ids::TransitServiceId::new("bus"),
                    TransitMode::Bus,
                    "bus",
                    "b",
                    vec![BoardingPointId::new("a"), BoardingPointId::new("b")],
                ),
                TransitService::new(
                    crate::domain::ids::TransitServiceId::new("metro"),
                    TransitMode::Metro,
                    "metro",
                    "d",
                    vec![BoardingPointId::new("c"), BoardingPointId::new("d")],
                ),
            ],
            Vec::new(),
        );

        assert!(stop_coverage_dominates(
            &network,
            &BoardingPointId::new("a"),
            &BoardingPointId::new("b")
        ));
        assert!(!stop_coverage_dominates(
            &network,
            &BoardingPointId::new("a"),
            &BoardingPointId::new("c")
        ));
    }

    #[test]
    fn nearby_stops_reserves_a_transit_mode_before_the_nearest_stop_limit() {
        let origin = RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        };
        let metro_coordinate = RequestCoordinate {
            latitude: 25.0004,
            longitude: 121.5,
        };
        let mut stops = (0..24)
            .map(|index| stop_at(&format!("bus-stop-{index}"), origin))
            .collect::<Vec<_>>();
        stops.push(stop_at("metro-stop", metro_coordinate));
        let mut services = Vec::new();
        for index in 0..24 {
            services.push(TransitService::new(
                crate::domain::ids::TransitServiceId::new(format!("bus-{index}")),
                TransitMode::Bus,
                "bus",
                "bus",
                vec![
                    BoardingPointId::new(format!("bus-stop-{index}")),
                    BoardingPointId::new(format!("bus-end-{index}")),
                ],
            ));
        }
        stops.extend((0..24).map(|index| {
            stop_at(
                &format!("bus-end-{index}"),
                RequestCoordinate {
                    latitude: 25.1,
                    longitude: 121.5,
                },
            )
        }));
        services.push(TransitService::new(
            crate::domain::ids::TransitServiceId::new("red-line"),
            TransitMode::Metro,
            "red",
            "red",
            vec![
                BoardingPointId::new("metro-stop"),
                BoardingPointId::new("metro-end"),
            ],
        ));
        stops.push(stop_at(
            "metro-end",
            RequestCoordinate {
                latitude: 25.1,
                longitude: 121.5,
            },
        ));
        let network = TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            stops,
            services,
            Vec::new(),
        );
        let limits = RoutingLimits::default();
        let nearby = nearby_stops(
            &network,
            Coordinate::try_new(origin.latitude, origin.longitude).unwrap(),
            24,
            &limits,
        );

        assert_eq!(nearby.len(), 24);
        assert!(
            nearby
                .iter()
                .any(|(stop_id, _)| stop_id.as_str() == "metro-stop")
        );
    }
}
