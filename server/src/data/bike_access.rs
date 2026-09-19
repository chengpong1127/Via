//! Static walking access between transit boarding points and bike stations.

use std::time::Instant;

use futures_util::{StreamExt, stream};

use crate::{
    domain::{
        bikeshare::{BikeShareNetwork, BikeStationAccess, BikeWalkingAccess},
        ids::{BikeStationId, BoardingPointId},
        transit::TransitNetwork,
    },
    street::{
        Coordinate, MatrixCell, MatrixLimits, MatrixOrientation, MatrixQuery, Metres,
        RoutingConstraints, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BikeAccessPolicy {
    pub max_geometric_radius_metres: u32,
    pub max_walking_duration_seconds: u32,
    pub max_walking_distance_metres: u32,
    pub max_geometric_candidates_per_boarding_point: usize,
    pub max_access_stations_per_boarding_point: usize,
    pub max_matrix_concurrency: usize,
}

impl Default for BikeAccessPolicy {
    fn default() -> Self {
        Self {
            max_geometric_radius_metres: 300,
            max_walking_duration_seconds: 8 * 60,
            max_walking_distance_metres: 800,
            max_geometric_candidates_per_boarding_point: 16,
            max_access_stations_per_boarding_point: 8,
            max_matrix_concurrency: 4,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BikeAccessDiagnostics {
    pub boarding_points_processed: usize,
    pub geometric_candidate_pairs: usize,
    pub walking_matrix_requests: usize,
    pub reachable_boarding_point_to_station_links: usize,
    pub reachable_station_to_boarding_point_links: usize,
    pub retained_access_links: usize,
    pub build_wall_clock_milliseconds: u64,
}

#[derive(Clone)]
struct Candidate {
    id: BikeStationId,
    coordinate: Coordinate,
    geometric_distance_metres: u64,
}

struct Source {
    boarding_point_id: BoardingPointId,
    coordinate: Coordinate,
    candidates: Vec<Candidate>,
}

struct SourceDiscovery {
    boarding_point_id: BoardingPointId,
    candidate_pairs: usize,
    matrix_requests: usize,
    reachable_boarding_point_to_station_links: usize,
    reachable_station_to_boarding_point_links: usize,
    links: Vec<(BikeStationAccess, u64)>,
}

pub async fn precompute_bike_access(
    transit: &TransitNetwork,
    bike_share: BikeShareNetwork,
    street: &dyn StreetRouter,
    policy: BikeAccessPolicy,
) -> Result<(BikeShareNetwork, BikeAccessDiagnostics), StreetRoutingError> {
    let started = Instant::now();
    validate_policy(policy, street.matrix_limits())?;
    let sources = source_candidates(transit, &bike_share, policy)?;
    let diagnostics_boarding_points = sources.len();
    let results = stream::iter(sources)
        .map(|source| discover_source(street, source, policy))
        .buffer_unordered(policy.max_matrix_concurrency)
        .collect::<Vec<_>>()
        .await;

    let mut discoveries = Vec::with_capacity(results.len());
    for result in results {
        discoveries.push(result?);
    }
    discoveries.sort_by(|left, right| {
        left.boarding_point_id
            .as_str()
            .cmp(right.boarding_point_id.as_str())
    });

    let mut access_links = Vec::new();
    let mut diagnostics = BikeAccessDiagnostics {
        boarding_points_processed: diagnostics_boarding_points,
        ..BikeAccessDiagnostics::default()
    };
    for discovery in discoveries {
        diagnostics.geometric_candidate_pairs += discovery.candidate_pairs;
        diagnostics.walking_matrix_requests += discovery.matrix_requests;
        diagnostics.reachable_boarding_point_to_station_links +=
            discovery.reachable_boarding_point_to_station_links;
        diagnostics.reachable_station_to_boarding_point_links +=
            discovery.reachable_station_to_boarding_point_links;

        let mut links = discovery.links;
        links.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then(left.0.station_id.as_str().cmp(right.0.station_id.as_str()))
        });
        links.truncate(policy.max_access_stations_per_boarding_point);
        diagnostics.retained_access_links += links.len();
        access_links.extend(links.into_iter().map(|(link, _)| link));
    }
    diagnostics.build_wall_clock_milliseconds =
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    Ok((
        bike_share.with_access_links(access_links).canonicalize(),
        diagnostics,
    ))
}

fn validate_policy(
    policy: BikeAccessPolicy,
    matrix_limits: MatrixLimits,
) -> Result<(), StreetRoutingError> {
    if policy.max_geometric_radius_metres == 0
        || policy.max_walking_duration_seconds == 0
        || policy.max_walking_distance_metres == 0
        || policy.max_geometric_candidates_per_boarding_point == 0
        || policy.max_access_stations_per_boarding_point == 0
        || policy.max_matrix_concurrency == 0
    {
        return Err(StreetRoutingError::InvalidRequest {
            reason: "bike access policy limits must be positive".into(),
        });
    }
    if policy.max_geometric_candidates_per_boarding_point > matrix_limits.max_sources
        || policy.max_geometric_candidates_per_boarding_point > matrix_limits.max_targets
    {
        return Err(StreetRoutingError::InvalidRequest {
            reason: "bike access candidate bound exceeds street matrix limits".into(),
        });
    }
    Ok(())
}

fn source_candidates(
    transit: &TransitNetwork,
    bike_share: &BikeShareNetwork,
    policy: BikeAccessPolicy,
) -> Result<Vec<Source>, StreetRoutingError> {
    transit
        .stops
        .iter()
        .map(|boarding_point| {
            let coordinate = Coordinate::try_new(
                boarding_point.coordinate.latitude,
                boarding_point.coordinate.longitude,
            )?;
            let mut candidates = bike_share
                .stations
                .iter()
                .filter_map(|station| {
                    let geometric_distance_metres = boarding_point
                        .coordinate
                        .planar_distance_metres(station.coordinate);
                    (geometric_distance_metres <= u64::from(policy.max_geometric_radius_metres))
                        .then(|| {
                            Coordinate::try_new(
                                station.coordinate.latitude,
                                station.coordinate.longitude,
                            )
                            .map(|coordinate| Candidate {
                                id: station.id.clone(),
                                coordinate,
                                geometric_distance_metres,
                            })
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            candidates.sort_by(|left, right| {
                left.geometric_distance_metres
                    .cmp(&right.geometric_distance_metres)
                    .then(left.id.as_str().cmp(right.id.as_str()))
            });
            candidates.truncate(policy.max_geometric_candidates_per_boarding_point);
            Ok(Source {
                boarding_point_id: boarding_point.id.clone(),
                coordinate,
                candidates,
            })
        })
        .collect()
}

async fn discover_source(
    street: &dyn StreetRouter,
    source: Source,
    policy: BikeAccessPolicy,
) -> Result<SourceDiscovery, StreetRoutingError> {
    if source.candidates.is_empty() {
        return Ok(SourceDiscovery {
            boarding_point_id: source.boarding_point_id,
            candidate_pairs: 0,
            matrix_requests: 0,
            reachable_boarding_point_to_station_links: 0,
            reachable_station_to_boarding_point_links: 0,
            links: Vec::new(),
        });
    }

    let targets = source
        .candidates
        .iter()
        .map(|candidate| candidate.coordinate)
        .collect::<Vec<_>>();
    let constraints = RoutingConstraints {
        time: None,
        max_distance: Some(Metres(policy.max_walking_distance_metres)),
    };
    let to_station = street
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            travel_mode: StreetTravelMode::Walking,
            sources: vec![source.coordinate],
            targets: targets.clone(),
            constraints,
        })
        .await?;
    let from_station = street
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::ManyToOne,
            travel_mode: StreetTravelMode::Walking,
            sources: targets,
            targets: vec![source.coordinate],
            constraints,
        })
        .await?;
    let to_station = one_to_many_cells(to_station, source.candidates.len())?;
    let from_station = many_to_one_cells(from_station, source.candidates.len())?;

    let candidate_pairs = source.candidates.len();
    let mut links = Vec::new();
    let mut reachable_boarding_point_to_station_links = 0;
    let mut reachable_station_to_boarding_point_links = 0;
    for (index, candidate) in source.candidates.into_iter().enumerate() {
        let walk_to_station = accepted_metric(
            to_station[index],
            policy.max_walking_duration_seconds,
            policy.max_walking_distance_metres,
        );
        let walk_from_station = accepted_metric(
            from_station[index],
            policy.max_walking_duration_seconds,
            policy.max_walking_distance_metres,
        );
        if walk_to_station.is_some() {
            reachable_boarding_point_to_station_links += 1;
        }
        if walk_from_station.is_some() {
            reachable_station_to_boarding_point_links += 1;
        }
        if let Some(rank) = best_rank(walk_to_station, walk_from_station) {
            links.push((
                BikeStationAccess::new(
                    source.boarding_point_id.clone(),
                    candidate.id,
                    walk_to_station,
                    walk_from_station,
                ),
                rank,
            ));
        }
    }

    Ok(SourceDiscovery {
        boarding_point_id: source.boarding_point_id,
        candidate_pairs,
        matrix_requests: 2,
        reachable_boarding_point_to_station_links,
        reachable_station_to_boarding_point_links,
        links,
    })
}

fn one_to_many_cells(
    result: crate::street::MatrixResult,
    expected_targets: usize,
) -> Result<Vec<MatrixCell>, StreetRoutingError> {
    if result.cells.len() != 1 || result.cells[0].len() != expected_targets {
        return Err(StreetRoutingError::MalformedResponse);
    }
    Ok(result.cells.into_iter().next().unwrap_or_default())
}

fn many_to_one_cells(
    result: crate::street::MatrixResult,
    expected_sources: usize,
) -> Result<Vec<MatrixCell>, StreetRoutingError> {
    if result.cells.len() != expected_sources || result.cells.iter().any(|row| row.len() != 1) {
        return Err(StreetRoutingError::MalformedResponse);
    }
    Ok(result
        .cells
        .into_iter()
        .map(|row| row.into_iter().next().unwrap_or(MatrixCell::Unreachable))
        .collect())
}

fn accepted_metric(
    cell: MatrixCell,
    max_duration_seconds: u32,
    max_distance_metres: u32,
) -> Option<BikeWalkingAccess> {
    let MatrixCell::Reachable { duration, distance } = cell else {
        return None;
    };
    (duration.0 > 0 && duration.0 <= max_duration_seconds && distance.0 <= max_distance_metres)
        .then(|| BikeWalkingAccess::new(duration.0, distance.0))
}

fn best_rank(
    walk_to_station: Option<BikeWalkingAccess>,
    walk_from_station: Option<BikeWalkingAccess>,
) -> Option<u64> {
    [walk_to_station, walk_from_station]
        .into_iter()
        .flatten()
        .map(|access| {
            u64::from(access.duration_seconds) * 1_000_000 + u64::from(access.distance_metres)
        })
        .min()
}

#[cfg(test)]
mod tests {
    use crate::{
        api::RequestCoordinate,
        domain::{
            bikeshare::{BikeShareNetwork, BikeStation, BikeWalkingAccess},
            ids::{BikeStationId, BoardingPointId},
            transit::{TransitNetwork, TransitStop},
        },
        street::{FakeStreetRouter, Metres, Seconds, StreetTravelMode},
    };

    use super::{BikeAccessPolicy, precompute_bike_access};

    fn coordinate(latitude: f64, longitude: f64) -> RequestCoordinate {
        RequestCoordinate {
            latitude,
            longitude,
        }
    }

    fn transit() -> TransitNetwork {
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![TransitStop::new(
                BoardingPointId::new("bp"),
                "BP",
                coordinate(25.0, 121.5),
            )],
            Vec::new(),
            Vec::new(),
        )
    }

    fn station(id: &str, latitude: f64, longitude: f64) -> BikeStation {
        BikeStation::new(BikeStationId::new(id), id, coordinate(latitude, longitude))
    }

    fn bike(stations: Vec<BikeStation>) -> BikeShareNetwork {
        BikeShareNetwork::new(stations)
    }

    fn street_with_directional_fixtures() -> FakeStreetRouter {
        let bp = crate::street::Coordinate::try_new(25.0, 121.5).unwrap();
        let station_a = crate::street::Coordinate::try_new(25.0005, 121.5).unwrap();
        FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                bp,
                station_a,
                Seconds(40),
                Metres(120),
            )
            .unreachable_leg(StreetTravelMode::Walking, station_a, bp)
            .build()
    }

    #[tokio::test]
    async fn geometric_candidates_are_bounded_and_directionality_is_preserved() {
        let result = precompute_bike_access(
            &transit(),
            bike(vec![
                station("a", 25.0005, 121.5),
                station("b", 25.001, 121.5),
            ]),
            &street_with_directional_fixtures(),
            BikeAccessPolicy {
                max_geometric_candidates_per_boarding_point: 1,
                ..BikeAccessPolicy::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(result.0.access_links.len(), 1);
        assert_eq!(
            result.0.access_links[0].walk_to_station,
            Some(BikeWalkingAccess::new(40, 120))
        );
        assert_eq!(result.0.access_links[0].walk_from_station, None);
        assert_eq!(result.1.geometric_candidate_pairs, 1);
        assert_eq!(result.1.walking_matrix_requests, 2);
    }

    #[tokio::test]
    async fn unreachable_and_over_limit_links_are_removed() {
        let bp = crate::street::Coordinate::try_new(25.0, 121.5).unwrap();
        let station_a = crate::street::Coordinate::try_new(25.0005, 121.5).unwrap();
        let station_b = crate::street::Coordinate::try_new(25.001, 121.5).unwrap();
        let street = FakeStreetRouter::builder()
            .unreachable_leg(StreetTravelMode::Walking, bp, station_a)
            .unreachable_leg(StreetTravelMode::Walking, station_a, bp)
            .matrix_leg(
                StreetTravelMode::Walking,
                bp,
                station_b,
                Seconds(900),
                Metres(120),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                station_b,
                bp,
                Seconds(40),
                Metres(120),
            )
            .build();
        let result = precompute_bike_access(
            &transit(),
            bike(vec![
                station("a", 25.0005, 121.5),
                station("b", 25.001, 121.5),
            ]),
            &street,
            BikeAccessPolicy::default(),
        )
        .await
        .unwrap();

        assert_eq!(result.0.access_links.len(), 1);
        assert_eq!(result.0.access_links[0].station_id.as_str(), "b");
        assert_eq!(result.1.reachable_boarding_point_to_station_links, 0);
        assert_eq!(result.1.reachable_station_to_boarding_point_links, 1);
    }

    #[tokio::test]
    async fn maximum_nearby_station_limit_is_respected_deterministically() {
        let bp = crate::street::Coordinate::try_new(25.0, 121.5).unwrap();
        let station_a = crate::street::Coordinate::try_new(25.0005, 121.5).unwrap();
        let station_b = crate::street::Coordinate::try_new(25.0006, 121.5).unwrap();
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                bp,
                station_a,
                Seconds(50),
                Metres(100),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                station_a,
                bp,
                Seconds(50),
                Metres(100),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                bp,
                station_b,
                Seconds(50),
                Metres(100),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                station_b,
                bp,
                Seconds(50),
                Metres(100),
            )
            .build();
        let result = precompute_bike_access(
            &transit(),
            bike(vec![
                station("b", 25.0006, 121.5),
                station("a", 25.0005, 121.5),
            ]),
            &street,
            BikeAccessPolicy {
                max_access_stations_per_boarding_point: 1,
                ..BikeAccessPolicy::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(result.0.access_links.len(), 1);
        assert_eq!(result.0.access_links[0].station_id.as_str(), "a");
        assert_eq!(result.1.retained_access_links, 1);
    }

    #[tokio::test]
    async fn repeated_preprocessing_has_identical_network_and_counts() {
        let bp = crate::street::Coordinate::try_new(25.0, 121.5).unwrap();
        let station_a = crate::street::Coordinate::try_new(25.0005, 121.5).unwrap();
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                bp,
                station_a,
                Seconds(40),
                Metres(100),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                station_a,
                bp,
                Seconds(45),
                Metres(110),
            )
            .build();
        let transit = transit();
        let bike = bike(vec![station("a", 25.0005, 121.5)]);
        let first =
            precompute_bike_access(&transit, bike.clone(), &street, BikeAccessPolicy::default())
                .await
                .unwrap();
        let second = precompute_bike_access(&transit, bike, &street, BikeAccessPolicy::default())
            .await
            .unwrap();

        assert_eq!(first.0, second.0);
        assert_eq!(
            (
                first.1.boarding_points_processed,
                first.1.geometric_candidate_pairs,
                first.1.walking_matrix_requests,
                first.1.reachable_boarding_point_to_station_links,
                first.1.reachable_station_to_boarding_point_links,
                first.1.retained_access_links,
            ),
            (
                second.1.boarding_points_processed,
                second.1.geometric_candidate_pairs,
                second.1.walking_matrix_requests,
                second.1.reachable_boarding_point_to_station_links,
                second.1.reachable_station_to_boarding_point_links,
                second.1.retained_access_links,
            )
        );
    }
}
