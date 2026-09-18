//! Static public-transit walking-transfer discovery.
//!
//! This build-stage enrichment keeps street routing behind the provider-neutral
//! `StreetRouter` boundary. The only persisted result is still
//! `TransitNetwork.transfer_edges`.

use std::collections::{BTreeMap, HashMap, HashSet};

use futures_util::{StreamExt, stream};

use crate::{
    domain::{
        ids::BoardingPointId,
        transit::{
            BikeShareConnector, BikeShareStation, TransferEdge, TransitMode, TransitNetwork,
            TransitStop,
        },
    },
    street::{
        Coordinate, MatrixCell, MatrixLimits, MatrixOrientation, MatrixQuery, Metres,
        RoutingConstraints, StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

/// Conservative policy for static walking-transfer discovery.
///
/// These values are build policy, not request-time routing configuration. The
/// geometric candidate bound limits matrix work; the inferred edge bound limits
/// the final graph fan-out after street metrics have been evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaticTransferPolicy {
    pub max_geometric_radius_metres: u32,
    pub max_walking_duration_seconds: u32,
    pub max_walking_distance_metres: u32,
    pub max_geometric_candidates_per_source: usize,
    pub max_inferred_transfers_per_source: usize,
    pub max_matrix_concurrency: usize,
}

/// Conservative build-time policy for static transit/BikeShare walking
/// connectors. This is deliberately separate from request-time routing limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaticBikeSharePolicy {
    pub max_geometric_radius_metres: u32,
    pub max_walking_duration_seconds: u32,
    pub max_walking_distance_metres: u32,
    pub max_geometric_candidates_per_stop: usize,
    pub max_connectors_per_stop: usize,
    pub max_matrix_concurrency: usize,
}

impl Default for StaticBikeSharePolicy {
    fn default() -> Self {
        Self {
            max_geometric_radius_metres: 500,
            max_walking_duration_seconds: 8 * 60,
            max_walking_distance_metres: 800,
            max_geometric_candidates_per_stop: 8,
            max_connectors_per_stop: 8,
            max_matrix_concurrency: 4,
        }
    }
}

impl Default for StaticTransferPolicy {
    fn default() -> Self {
        Self {
            max_geometric_radius_metres: 500,
            max_walking_duration_seconds: 8 * 60,
            max_walking_distance_metres: 800,
            max_geometric_candidates_per_source: 16,
            max_inferred_transfers_per_source: 12,
            max_matrix_concurrency: 4,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferDiscoveryDiagnostics {
    pub boarding_points: usize,
    pub source_points_processed: usize,
    pub geometric_candidate_pairs: usize,
    pub matrix_requests: usize,
    pub reachable_walking_pairs: usize,
    pub inferred_edges_kept: usize,
    pub explicit_edges_kept: usize,
    pub total_transfer_edges: usize,
    pub build_wall_clock_milliseconds: u64,
    pub bike_share_connectors: Option<BikeShareConnectorDiscoveryDiagnostics>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BikeShareConnectorDiscoveryDiagnostics {
    pub boarding_points: usize,
    pub geometric_candidate_pairs: usize,
    pub matrix_requests: usize,
    pub reachable_directions: usize,
    pub connectors_kept: usize,
}

#[derive(Clone)]
struct Candidate {
    id: BoardingPointId,
    coordinate: Coordinate,
    modes: Vec<TransitMode>,
}

struct SourceCandidates {
    id: BoardingPointId,
    coordinate: Coordinate,
    candidates: Vec<Candidate>,
}

struct SourceDiscovery {
    source_id: BoardingPointId,
    candidate_pairs: usize,
    matrix_requests: usize,
    reachable_walking_pairs: usize,
    inferred_edges: Vec<MeasuredEdge>,
}

struct MeasuredEdge {
    edge: TransferEdge,
    distance_metres: u32,
}

/// Add inferred walking transfers to a normalized network and return build
/// diagnostics. Existing transfer edges are treated as authoritative explicit
/// provider edges and cannot be overwritten by inferred metrics.
pub async fn enrich_network_with_static_transfers(
    mut network: TransitNetwork,
    street: &dyn StreetRouter,
    policy: StaticTransferPolicy,
) -> Result<(TransitNetwork, TransferDiscoveryDiagnostics), StreetRoutingError> {
    validate_policy(policy, street.matrix_limits())?;

    let sources = source_candidates(&network, policy)?;
    let limits = street.matrix_limits();
    let source_points_processed = sources.len();
    let results = stream::iter(sources)
        .map(|source| discover_source(street, source, policy, limits))
        .buffer_unordered(policy.max_matrix_concurrency)
        .collect::<Vec<_>>()
        .await;

    let mut discoveries = Vec::with_capacity(results.len());
    for result in results {
        discoveries.push(result?);
    }
    discoveries.sort_by(|left, right| left.source_id.as_str().cmp(right.source_id.as_str()));

    let mut explicit_by_pair = BTreeMap::<(String, String), TransferEdge>::new();
    for edge in &network.transfer_edges {
        let key = (edge.from.as_str().to_owned(), edge.to.as_str().to_owned());
        explicit_by_pair
            .entry(key)
            .and_modify(|existing| {
                if edge.duration_seconds < existing.duration_seconds {
                    *existing = edge.clone();
                }
            })
            .or_insert_with(|| edge.clone());
    }

    let explicit_edges_kept = explicit_by_pair.len();
    let mut merged = explicit_by_pair;
    let mut inferred_edges_kept = 0;
    for discovery in &mut discoveries {
        for measured in discovery.inferred_edges.drain(..) {
            let key = (
                measured.edge.from.as_str().to_owned(),
                measured.edge.to.as_str().to_owned(),
            );
            if let std::collections::btree_map::Entry::Vacant(entry) = merged.entry(key) {
                entry.insert(measured.edge);
                inferred_edges_kept += 1;
            }
        }
    }

    network.transfer_edges = merged.into_values().collect();
    network = network.canonicalize();
    let diagnostics = TransferDiscoveryDiagnostics {
        boarding_points: network.stops.len(),
        source_points_processed,
        geometric_candidate_pairs: discoveries
            .iter()
            .map(|discovery| discovery.candidate_pairs)
            .sum(),
        matrix_requests: discoveries
            .iter()
            .map(|discovery| discovery.matrix_requests)
            .sum(),
        reachable_walking_pairs: discoveries
            .iter()
            .map(|discovery| discovery.reachable_walking_pairs)
            .sum(),
        inferred_edges_kept,
        explicit_edges_kept,
        total_transfer_edges: network.transfer_edges.len(),
        build_wall_clock_milliseconds: 0,
        bike_share_connectors: None,
    };
    Ok((network, diagnostics))
}

/// Add static walking connectors between transit Boarding Points and nearby
/// BikeShare stations. Forward and reverse walking matrices are evaluated
/// independently and are persisted as one connector record per pair.
pub async fn enrich_network_with_static_bike_share_connectors(
    mut network: TransitNetwork,
    street: &dyn StreetRouter,
    policy: StaticBikeSharePolicy,
) -> Result<(TransitNetwork, BikeShareConnectorDiscoveryDiagnostics), StreetRoutingError> {
    validate_bike_policy(policy, street.matrix_limits())?;
    let limits = street.matrix_limits();
    let sources = network
        .stops
        .iter()
        .map(|stop| {
            Ok(BikeSourceCandidates {
                stop_id: stop.id.clone(),
                coordinate: coordinate(stop)?,
                stations: network
                    .bike_share_stations_within_radius(
                        stop.coordinate,
                        policy.max_geometric_radius_metres,
                    )
                    .into_iter()
                    .take(policy.max_geometric_candidates_per_stop)
                    .map(|station| {
                        Ok(BikeCandidate {
                            id: station.id.clone(),
                            coordinate: station_coordinate(station)?,
                        })
                    })
                    .collect::<Result<Vec<_>, StreetRoutingError>>()?,
            })
        })
        .collect::<Result<Vec<_>, StreetRoutingError>>()?;
    let results = stream::iter(sources)
        .map(|source| discover_bike_source(street, source, policy, limits))
        .buffer_unordered(policy.max_matrix_concurrency)
        .collect::<Vec<_>>()
        .await;

    let mut discoveries = Vec::with_capacity(results.len());
    for result in results {
        discoveries.push(result?);
    }
    discoveries.sort_by(|left, right| left.stop_id.as_str().cmp(right.stop_id.as_str()));

    let mut by_pair = BTreeMap::<(String, String), BikeShareConnector>::new();
    for connector in network.bike_share_connectors.drain(..) {
        by_pair.insert(
            (
                connector.stop_id.as_str().to_owned(),
                connector.station_id.as_str().to_owned(),
            ),
            connector,
        );
    }
    for discovery in &discoveries {
        for discovered in &discovery.connectors {
            let key = (
                discovered.stop_id.as_str().to_owned(),
                discovered.station_id.as_str().to_owned(),
            );
            match by_pair.get_mut(&key) {
                Some(existing) => merge_bike_connector(existing, discovered),
                None => {
                    by_pair.insert(key, discovered.clone());
                }
            }
        }
    }
    network.bike_share_connectors = by_pair.into_values().collect();
    network = network.canonicalize();
    let diagnostics = BikeShareConnectorDiscoveryDiagnostics {
        boarding_points: network.stops.len(),
        geometric_candidate_pairs: discoveries
            .iter()
            .map(|discovery| discovery.candidate_pairs)
            .sum(),
        matrix_requests: discoveries
            .iter()
            .map(|discovery| discovery.matrix_requests)
            .sum(),
        reachable_directions: discoveries
            .iter()
            .map(|discovery| discovery.reachable_directions)
            .sum(),
        connectors_kept: network.bike_share_connectors.len(),
    };
    Ok((network, diagnostics))
}

#[derive(Clone)]
struct BikeCandidate {
    id: crate::domain::ids::BikeShareStationId,
    coordinate: Coordinate,
}

struct BikeSourceCandidates {
    stop_id: BoardingPointId,
    coordinate: Coordinate,
    stations: Vec<BikeCandidate>,
}

struct BikeSourceDiscovery {
    stop_id: BoardingPointId,
    candidate_pairs: usize,
    matrix_requests: usize,
    reachable_directions: usize,
    connectors: Vec<BikeShareConnector>,
}

async fn discover_bike_source(
    street: &dyn StreetRouter,
    source: BikeSourceCandidates,
    policy: StaticBikeSharePolicy,
    limits: MatrixLimits,
) -> Result<BikeSourceDiscovery, StreetRoutingError> {
    let candidate_pairs = source.stations.len();
    if source.stations.is_empty() {
        return Ok(BikeSourceDiscovery {
            stop_id: source.stop_id,
            candidate_pairs,
            matrix_requests: 0,
            reachable_directions: 0,
            connectors: Vec::new(),
        });
    }

    let mut matrix_requests = 0;
    let mut reachable_directions = 0;
    let mut connectors = source
        .stations
        .iter()
        .map(|station| BikeShareConnector::new(source.stop_id.clone(), station.id.clone()))
        .collect::<Vec<_>>();

    for candidates in source.stations.chunks(limits.max_targets) {
        matrix_requests += 1;
        let matrix = street
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![source.coordinate],
                targets: candidates
                    .iter()
                    .map(|candidate| candidate.coordinate)
                    .collect(),
                constraints: RoutingConstraints {
                    max_distance: Some(Metres(policy.max_walking_distance_metres)),
                    ..RoutingConstraints::default()
                },
            })
            .await?;
        let Some(cells) = matrix.cells.first() else {
            return Err(StreetRoutingError::MalformedResponse);
        };
        if matrix.cells.len() != 1 || cells.len() != candidates.len() {
            return Err(StreetRoutingError::MalformedResponse);
        }
        for (candidate, cell) in candidates.iter().zip(cells) {
            let Some((seconds, metres)) = bike_walking_metric(cell, policy) else {
                continue;
            };
            reachable_directions += 1;
            if let Some(connector) = connectors
                .iter_mut()
                .find(|connector| connector.station_id == candidate.id)
            {
                connector.stop_to_station_seconds = Some(seconds);
                connector.stop_to_station_metres = Some(metres);
            }
        }
    }

    for candidates in source.stations.chunks(limits.max_sources) {
        matrix_requests += 1;
        let matrix = street
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToOne,
                travel_mode: StreetTravelMode::Walking,
                sources: candidates
                    .iter()
                    .map(|candidate| candidate.coordinate)
                    .collect(),
                targets: vec![source.coordinate],
                constraints: RoutingConstraints {
                    max_distance: Some(Metres(policy.max_walking_distance_metres)),
                    ..RoutingConstraints::default()
                },
            })
            .await?;
        if matrix.cells.len() != candidates.len()
            || matrix.cells.iter().any(|cells| cells.len() != 1)
        {
            return Err(StreetRoutingError::MalformedResponse);
        }
        for (candidate, cells) in candidates.iter().zip(&matrix.cells) {
            let Some((seconds, metres)) = bike_walking_metric(&cells[0], policy) else {
                continue;
            };
            reachable_directions += 1;
            if let Some(connector) = connectors
                .iter_mut()
                .find(|connector| connector.station_id == candidate.id)
            {
                connector.station_to_stop_seconds = Some(seconds);
                connector.station_to_stop_metres = Some(metres);
            }
        }
    }

    connectors.retain(BikeShareConnector::has_direction);
    connectors.sort_by(|left, right| {
        connector_sort_key(left)
            .cmp(&connector_sort_key(right))
            .then(left.station_id.as_str().cmp(right.station_id.as_str()))
    });
    connectors.truncate(policy.max_connectors_per_stop);
    Ok(BikeSourceDiscovery {
        stop_id: source.stop_id,
        candidate_pairs,
        matrix_requests,
        reachable_directions,
        connectors,
    })
}

fn bike_walking_metric(cell: &MatrixCell, policy: StaticBikeSharePolicy) -> Option<(u32, u32)> {
    let MatrixCell::Reachable { duration, distance } = cell else {
        return None;
    };
    if duration.0 == 0
        || duration.0 > policy.max_walking_duration_seconds
        || distance.0 == 0
        || distance.0 > policy.max_walking_distance_metres
    {
        return None;
    }
    Some((duration.0, distance.0))
}

fn connector_sort_key(connector: &BikeShareConnector) -> (u32, u32) {
    (
        connector
            .stop_to_station_seconds
            .or(connector.station_to_stop_seconds)
            .unwrap_or(u32::MAX),
        connector
            .stop_to_station_metres
            .or(connector.station_to_stop_metres)
            .unwrap_or(u32::MAX),
    )
}

fn merge_bike_connector(existing: &mut BikeShareConnector, discovered: &BikeShareConnector) {
    if existing.stop_to_station_seconds.is_none() {
        existing.stop_to_station_seconds = discovered.stop_to_station_seconds;
        existing.stop_to_station_metres = discovered.stop_to_station_metres;
    }
    if existing.station_to_stop_seconds.is_none() {
        existing.station_to_stop_seconds = discovered.station_to_stop_seconds;
        existing.station_to_stop_metres = discovered.station_to_stop_metres;
    }
}

fn validate_bike_policy(
    policy: StaticBikeSharePolicy,
    limits: MatrixLimits,
) -> Result<(), StreetRoutingError> {
    if policy.max_geometric_radius_metres == 0
        || policy.max_walking_duration_seconds == 0
        || policy.max_walking_distance_metres == 0
        || policy.max_geometric_candidates_per_stop == 0
        || policy.max_connectors_per_stop == 0
        || policy.max_matrix_concurrency == 0
        || limits.max_sources == 0
        || limits.max_targets == 0
    {
        return Err(StreetRoutingError::InvalidRequest {
            reason: "static BikeShare policy and matrix bounds must be greater than zero".into(),
        });
    }
    Ok(())
}

fn validate_policy(
    policy: StaticTransferPolicy,
    limits: MatrixLimits,
) -> Result<(), StreetRoutingError> {
    if policy.max_geometric_radius_metres == 0
        || policy.max_walking_duration_seconds == 0
        || policy.max_walking_distance_metres == 0
        || policy.max_geometric_candidates_per_source == 0
        || policy.max_matrix_concurrency == 0
        || limits.max_sources == 0
        || limits.max_targets == 0
    {
        return Err(StreetRoutingError::InvalidRequest {
            reason: "static transfer policy and matrix bounds must be greater than zero".into(),
        });
    }
    Ok(())
}

fn source_candidates(
    network: &TransitNetwork,
    policy: StaticTransferPolicy,
) -> Result<Vec<SourceCandidates>, StreetRoutingError> {
    let mut modes_by_stop = HashMap::<BoardingPointId, Vec<TransitMode>>::new();
    for service in &network.services {
        for stop_id in &service.pattern {
            let modes = modes_by_stop.entry(stop_id.clone()).or_default();
            if !modes.contains(&service.transit_mode) {
                modes.push(service.transit_mode);
            }
        }
    }
    for modes in modes_by_stop.values_mut() {
        modes.sort_by_key(|mode| *mode as u8);
    }

    network
        .stops
        .iter()
        .map(|source| {
            let source_coordinate = coordinate(source)?;
            let mut seen = HashSet::new();
            let mut candidates = network
                .stops_within_radius(source.coordinate, policy.max_geometric_radius_metres)
                .into_iter()
                .filter(|candidate| candidate.id != source.id)
                .filter(|candidate| seen.insert(candidate.id.clone()))
                .map(|candidate| {
                    Ok((
                        source
                            .coordinate
                            .planar_distance_metres(candidate.coordinate),
                        Candidate {
                            id: candidate.id.clone(),
                            coordinate: coordinate(candidate)?,
                            modes: modes_by_stop
                                .get(&candidate.id)
                                .cloned()
                                .unwrap_or_default(),
                        },
                    ))
                })
                .collect::<Result<Vec<_>, StreetRoutingError>>()?;
            candidates.sort_by(|left, right| {
                left.0
                    .cmp(&right.0)
                    .then(left.1.id.as_str().cmp(right.1.id.as_str()))
            });
            let mut selected = Vec::with_capacity(policy.max_geometric_candidates_per_source);
            let mut selected_ids = HashSet::new();
            for mode in [
                TransitMode::Bus,
                TransitMode::Metro,
                TransitMode::Rail,
                TransitMode::HighSpeedRail,
            ] {
                if selected.len() >= policy.max_geometric_candidates_per_source {
                    break;
                }
                if let Some(candidate) = candidates
                    .iter()
                    .find(|(_, candidate)| candidate.modes.contains(&mode))
                {
                    if selected_ids.insert(candidate.1.id.clone()) {
                        selected.push(candidate.clone());
                    }
                }
            }
            for candidate in candidates {
                if selected.len() >= policy.max_geometric_candidates_per_source {
                    break;
                }
                if selected_ids.insert(candidate.1.id.clone()) {
                    selected.push(candidate);
                }
            }
            Ok(SourceCandidates {
                id: source.id.clone(),
                coordinate: source_coordinate,
                candidates: selected
                    .into_iter()
                    .map(|(_, candidate)| candidate)
                    .collect(),
            })
        })
        .collect()
}

async fn discover_source(
    street: &dyn StreetRouter,
    source: SourceCandidates,
    policy: StaticTransferPolicy,
    limits: MatrixLimits,
) -> Result<SourceDiscovery, StreetRoutingError> {
    let candidate_pairs = source.candidates.len();
    let mut matrix_requests = 0;
    let mut reachable_walking_pairs = 0;
    let mut measured = Vec::new();

    for candidates in source.candidates.chunks(limits.max_targets) {
        matrix_requests += 1;
        let matrix = street
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![source.coordinate],
                targets: candidates
                    .iter()
                    .map(|candidate| candidate.coordinate)
                    .collect(),
                constraints: RoutingConstraints {
                    max_distance: Some(Metres(policy.max_walking_distance_metres)),
                    ..RoutingConstraints::default()
                },
            })
            .await?;
        let Some(cells) = matrix.cells.first() else {
            return Err(StreetRoutingError::MalformedResponse);
        };
        if matrix.cells.len() != 1 || cells.len() != candidates.len() {
            return Err(StreetRoutingError::MalformedResponse);
        }
        for (candidate, cell) in candidates.iter().zip(cells) {
            let MatrixCell::Reachable { duration, distance } = cell else {
                continue;
            };
            reachable_walking_pairs += 1;
            if duration.0 == 0
                || duration.0 > policy.max_walking_duration_seconds
                || distance.0 > policy.max_walking_distance_metres
            {
                continue;
            }
            measured.push(MeasuredEdge {
                edge: TransferEdge::new(source.id.clone(), candidate.id.clone(), duration.0),
                distance_metres: distance.0,
            });
        }
    }

    measured.sort_by(|left, right| {
        left.edge
            .duration_seconds
            .cmp(&right.edge.duration_seconds)
            .then(left.distance_metres.cmp(&right.distance_metres))
            .then(left.edge.to.as_str().cmp(right.edge.to.as_str()))
    });
    measured.truncate(policy.max_inferred_transfers_per_source);

    Ok(SourceDiscovery {
        source_id: source.id,
        candidate_pairs,
        matrix_requests,
        reachable_walking_pairs,
        inferred_edges: measured,
    })
}

fn coordinate(stop: &TransitStop) -> Result<Coordinate, StreetRoutingError> {
    Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)
}

fn station_coordinate(station: &BikeShareStation) -> Result<Coordinate, StreetRoutingError> {
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude)
}

#[cfg(test)]
mod tests {
    use crate::{
        domain::{
            ids::{BikeShareStationId, BoardingPointId},
            location::RequestCoordinate,
            transit::{
                BikeShareStation, TransferEdge, TransitMode, TransitNetwork, TransitService,
                TransitStop,
            },
        },
        street::{FakeStreetRouter, Metres, Seconds, StreetTravelMode},
    };

    use super::{StaticTransferPolicy, enrich_network_with_static_transfers};

    #[tokio::test]
    async fn discovers_directional_transit_to_bike_connectors_in_both_matrix_directions() {
        let transit = network(vec![("A", 25.0, 121.0)], Vec::new());
        let station_coordinate = coordinate(25.0005, 121.0);
        let mut transit = transit;
        transit.bike_share_stations.push(BikeShareStation::new(
            BikeShareStationId::new("station"),
            "station",
            station_coordinate,
        ));
        let stop = street_coordinate(coordinate(25.0, 121.0));
        let station = street_coordinate(station_coordinate);
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                stop,
                station,
                Seconds(90),
                Metres(120),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                station,
                stop,
                Seconds(110),
                Metres(140),
            )
            .build();

        let (result, diagnostics) = super::enrich_network_with_static_bike_share_connectors(
            transit,
            &street,
            super::StaticBikeSharePolicy::default(),
        )
        .await
        .unwrap();

        let connector = &result.bike_share_connectors[0];
        assert_eq!(connector.stop_to_station_seconds, Some(90));
        assert_eq!(connector.station_to_stop_seconds, Some(110));
        assert_eq!(connector.stop_to_station_metres, Some(120));
        assert_eq!(connector.station_to_stop_metres, Some(140));
        assert_eq!(diagnostics.matrix_requests, 2);
    }

    fn coordinate(latitude: f64, longitude: f64) -> RequestCoordinate {
        RequestCoordinate {
            latitude,
            longitude,
        }
    }

    fn network(stops: Vec<(&str, f64, f64)>, transfer_edges: Vec<TransferEdge>) -> TransitNetwork {
        let stops = stops
            .into_iter()
            .map(|(id, latitude, longitude)| {
                TransitStop::new(
                    BoardingPointId::new(id),
                    id,
                    coordinate(latitude, longitude),
                )
            })
            .collect::<Vec<_>>();
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            stops,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .with_transfer_edges(transfer_edges)
    }

    fn street_coordinate(value: RequestCoordinate) -> crate::street::Coordinate {
        crate::street::Coordinate::try_new(value.latitude, value.longitude).unwrap()
    }

    #[tokio::test]
    async fn keeps_reachable_candidates_and_rejects_unreachable_candidates() {
        let transit = network(
            vec![
                ("A", 25.0, 121.0),
                ("B", 25.0005, 121.0),
                ("C", 25.0006, 121.0),
            ],
            Vec::new(),
        );
        let a = street_coordinate(coordinate(25.0, 121.0));
        let b = street_coordinate(coordinate(25.0005, 121.0));
        let c = street_coordinate(coordinate(25.0006, 121.0));
        let street = FakeStreetRouter::builder()
            .matrix_leg(StreetTravelMode::Walking, a, b, Seconds(120), Metres(150))
            .unreachable_leg(StreetTravelMode::Walking, a, c)
            .build();

        let (result, _) =
            enrich_network_with_static_transfers(transit, &street, StaticTransferPolicy::default())
                .await
                .unwrap();
        assert_eq!(
            result.transfer_seconds(&BoardingPointId::new("A"), &BoardingPointId::new("B")),
            Some(120)
        );
        assert_eq!(
            result.transfer_seconds(&BoardingPointId::new("A"), &BoardingPointId::new("C")),
            None
        );
    }

    #[tokio::test]
    async fn preserves_directionality_and_explicit_precedence() {
        let transit = network(
            vec![("A", 25.0, 121.0), ("B", 25.0005, 121.0)],
            vec![TransferEdge::new(
                BoardingPointId::new("A"),
                BoardingPointId::new("B"),
                240,
            )],
        );
        let a = street_coordinate(coordinate(25.0, 121.0));
        let b = street_coordinate(coordinate(25.0005, 121.0));
        let street = FakeStreetRouter::builder()
            .matrix_leg(StreetTravelMode::Walking, a, b, Seconds(100), Metres(150))
            .unreachable_leg(StreetTravelMode::Walking, b, a)
            .build();

        let (result, diagnostics) =
            enrich_network_with_static_transfers(transit, &street, StaticTransferPolicy::default())
                .await
                .unwrap();
        assert_eq!(
            result.transfer_seconds(&BoardingPointId::new("A"), &BoardingPointId::new("B")),
            Some(240)
        );
        assert_eq!(
            result.transfer_seconds(&BoardingPointId::new("B"), &BoardingPointId::new("A")),
            None
        );
        assert_eq!(diagnostics.explicit_edges_kept, 1);
        assert_eq!(diagnostics.inferred_edges_kept, 0);
    }

    #[tokio::test]
    async fn caps_inferred_edges_without_removing_explicit_edges_or_adding_bikes() {
        let ids = [
            "B00", "B01", "B02", "B03", "B04", "B05", "B06", "B07", "B08", "B09", "B10", "B11",
            "B12", "B13",
        ];
        let mut stops = vec![("A", 25.0, 121.0)];
        stops.extend(
            ids.iter()
                .zip(0_u32..14)
                .map(|(id, index)| (*id, 25.0001 + f64::from(index) * 0.00001, 121.0)),
        );
        let explicit =
            TransferEdge::new(BoardingPointId::new("A"), BoardingPointId::new("B13"), 999);
        let mut transit = network(stops, vec![explicit.clone()]).with_default_transfer_seconds(120);
        transit.bike_share_stations.push(BikeShareStation::new(
            BikeShareStationId::new("bike-1"),
            "bike",
            coordinate(25.0002, 121.0),
        ));
        let a = street_coordinate(coordinate(25.0, 121.0));
        let mut builder = FakeStreetRouter::builder();
        for index in 0_u32..14 {
            let target = street_coordinate(coordinate(25.0001 + f64::from(index) * 0.00001, 121.0));
            builder = builder.matrix_leg(
                StreetTravelMode::Walking,
                a,
                target,
                Seconds(100 + index),
                Metres(150 + index),
            );
        }
        let (result, diagnostics) = enrich_network_with_static_transfers(
            transit,
            &builder.build(),
            StaticTransferPolicy::default(),
        )
        .await
        .unwrap();
        let outgoing = result
            .transfer_edges
            .iter()
            .filter(|edge| edge.from == BoardingPointId::new("A"))
            .collect::<Vec<_>>();
        assert_eq!(outgoing.len(), 13);
        assert!(
            outgoing
                .iter()
                .any(|edge| edge.to == explicit.to && edge.duration_seconds == 999)
        );
        assert_eq!(diagnostics.inferred_edges_kept, 12);
        assert!(
            !result
                .transfer_edges
                .iter()
                .any(|edge| { edge.from.as_str() == "bike-1" || edge.to.as_str() == "bike-1" })
        );
    }

    #[tokio::test]
    async fn retains_a_nearby_candidate_for_a_less_common_transit_mode() {
        let mut stops = vec![("A", 25.0, 121.0)];
        stops.extend((0_u32..5).map(|index| {
            (
                match index {
                    0 => "B00",
                    1 => "B01",
                    2 => "B02",
                    3 => "B03",
                    _ => "B04",
                },
                25.0001 + f64::from(index) * 0.0001,
                121.0,
            )
        }));
        stops.push(("H00", 25.002, 121.0));
        stops.push(("H01", 25.0021, 121.0));
        let mut transit = network(stops, Vec::new());
        transit.services = vec![TransitService::new(
            crate::domain::ids::TransitServiceId::new("high-speed"),
            TransitMode::HighSpeedRail,
            "HSR",
            "Taipei",
            vec![BoardingPointId::new("H00"), BoardingPointId::new("H01")],
        )];

        let a = street_coordinate(coordinate(25.0, 121.0));
        let h = street_coordinate(coordinate(25.002, 121.0));
        let street = FakeStreetRouter::builder()
            .matrix_leg(StreetTravelMode::Walking, a, h, Seconds(180), Metres(250))
            .build();
        let policy = StaticTransferPolicy {
            max_geometric_candidates_per_source: 4,
            max_inferred_transfers_per_source: 4,
            ..StaticTransferPolicy::default()
        };

        let (result, _) = enrich_network_with_static_transfers(transit, &street, policy)
            .await
            .unwrap();

        assert_eq!(
            result.transfer_seconds(&BoardingPointId::new("A"), &BoardingPointId::new("H00")),
            Some(180)
        );
    }

    #[tokio::test]
    async fn repeated_discovery_is_deterministic() {
        let transit = network(
            vec![
                ("A", 25.0, 121.0),
                ("B", 25.0005, 121.0),
                ("C", 25.0006, 121.0),
            ],
            Vec::new(),
        );
        let a = street_coordinate(coordinate(25.0, 121.0));
        let b = street_coordinate(coordinate(25.0005, 121.0));
        let c = street_coordinate(coordinate(25.0006, 121.0));
        let street = FakeStreetRouter::builder()
            .matrix_leg(StreetTravelMode::Walking, a, b, Seconds(120), Metres(150))
            .matrix_leg(StreetTravelMode::Walking, a, c, Seconds(140), Metres(170))
            .matrix_leg(StreetTravelMode::Walking, b, a, Seconds(120), Metres(150))
            .matrix_leg(StreetTravelMode::Walking, b, c, Seconds(110), Metres(140))
            .matrix_leg(StreetTravelMode::Walking, c, a, Seconds(140), Metres(170))
            .matrix_leg(StreetTravelMode::Walking, c, b, Seconds(110), Metres(140))
            .build();
        let first = enrich_network_with_static_transfers(
            transit.clone(),
            &street,
            StaticTransferPolicy::default(),
        )
        .await
        .unwrap();
        let second =
            enrich_network_with_static_transfers(transit, &street, StaticTransferPolicy::default())
                .await
                .unwrap();
        assert_eq!(first, second);
    }
}
