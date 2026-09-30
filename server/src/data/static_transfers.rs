use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Instant,
};

use futures_util::{StreamExt, stream};

use crate::{
    domain::{
        ids::BoardingPointId,
        transit::{TransferEdge, TransitMode, TransitNetwork},
    },
    street::{
        Coordinate, MatrixCell, MatrixOrientation, MatrixQuery, Metres, RoutingConstraints,
        StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

const RADIUS_METRES: f64 = 500.0;
const MAX_DURATION_SECONDS: u32 = 480;
const MAX_DISTANCE_METRES: u32 = 800;
const MAX_OUTGOING: usize = 12;
const CELL_DEGREES: f64 = 0.004;
const CONCURRENCY: usize = 8;

#[derive(Clone, Copy)]
struct WalkingTransferMetrics {
    duration_seconds: u32,
    distance_metres: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferStatistics {
    pub boarding_points: usize,
    pub geographic_candidate_pairs: usize,
    pub matrix_pairs_queried: usize,
    pub reachable_walking_pairs: usize,
    pub removed_by_limits: usize,
    pub removed_by_fanout: usize,
    pub retained_edges: usize,
    pub elapsed_milliseconds: u128,
    pub by_mode: BTreeMap<String, usize>,
}

fn cell(lat: f64, lon: f64) -> (i32, i32) {
    (
        (lat / CELL_DEGREES).floor() as i32,
        (lon / CELL_DEGREES).floor() as i32,
    )
}

fn coordinate(network: &TransitNetwork, index: usize) -> Coordinate {
    let point = network.stops[index].coordinate;
    Coordinate::try_new(point.latitude, point.longitude).expect("validated GTFS stop coordinate")
}

fn mode_name(mode: TransitMode) -> &'static str {
    match mode {
        TransitMode::Bus => "Bus",
        TransitMode::Metro => "Metro",
        TransitMode::Rail => "Rail",
        TransitMode::HighSpeedRail => "THSR",
    }
}

pub async fn enrich_network_with_static_transfers(
    network: &mut TransitNetwork,
    street: &dyn StreetRouter,
) -> Result<TransferStatistics, StreetRoutingError> {
    let started = Instant::now();
    let used = network
        .patterns
        .iter()
        .flat_map(|pattern| pattern.stops.iter())
        .collect::<HashSet<_>>();
    let indices = network
        .stops
        .iter()
        .enumerate()
        .filter_map(|(index, stop)| used.contains(&stop.id).then_some(index))
        .collect::<Vec<_>>();
    let mut stats = TransferStatistics {
        boarding_points: indices.len(),
        ..Default::default()
    };
    let mut grid = HashMap::<(i32, i32), Vec<usize>>::new();
    for &index in &indices {
        let point = network.stops[index].coordinate;
        grid.entry(cell(point.latitude, point.longitude))
            .or_default()
            .push(index);
    }

    let limits = street.matrix_limits();
    if limits.max_targets == 0 || limits.max_sources == 0 {
        return Err(StreetRoutingError::InvalidRequest {
            reason: "street matrix limits must be positive".into(),
        });
    }
    let mut best = HashMap::<(BoardingPointId, BoardingPointId), WalkingTransferMetrics>::new();

    for source_group in indices.chunks(128) {
        let mut requests = Vec::new();
        for &source in source_group {
            let origin = network.stops[source].coordinate;
            let (row, column) = cell(origin.latitude, origin.longitude);
            let mut targets = Vec::new();

            for dy in -3..=3 {
                for dx in -3..=3 {
                    if let Some(neighbors) = grid.get(&(row + dy, column + dx)) {
                        for &target in neighbors {
                            if target != source
                                && origin
                                    .haversine_distance_metres(network.stops[target].coordinate)
                                    <= RADIUS_METRES
                            {
                                targets.push(target);
                            }
                        }
                    }
                }
            }
            targets.sort_unstable();
            stats.geographic_candidate_pairs += targets.len();
            for chunk in targets.chunks(limits.max_targets) {
                requests.push((source, chunk.to_vec()));
            }
        }
        let network_ref: &TransitNetwork = network;
        let results = stream::iter(requests.into_iter().map(|(source, targets)| async move {
            let query = MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![coordinate(network_ref, source)],
                targets: targets
                    .iter()
                    .map(|&index| coordinate(network_ref, index))
                    .collect(),
                constraints: RoutingConstraints {
                    time: None,
                    max_distance: Some(Metres(MAX_DISTANCE_METRES)),
                },
            };
            let matrix = street.matrix(query).await?;
            Ok::<_, StreetRoutingError>((source, targets, matrix))
        }))
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        for result in results {
            let (source, targets, matrix) = result?;
            stats.matrix_pairs_queried += targets.len();
            let [cells] = matrix.cells.as_slice() else {
                return Err(StreetRoutingError::MalformedResponse);
            };
            if cells.len() != targets.len() {
                return Err(StreetRoutingError::MalformedResponse);
            }
            for (target, value) in targets.into_iter().zip(cells) {
                if let MatrixCell::Reachable { duration, distance } = value {
                    stats.reachable_walking_pairs += 1;
                    if duration.0 == 0
                        || duration.0 > MAX_DURATION_SECONDS
                        || distance.0 > MAX_DISTANCE_METRES
                    {
                        stats.removed_by_limits += 1;
                        continue;
                    }
                    let key = (
                        network.stops[source].id.clone(),
                        network.stops[target].id.clone(),
                    );
                    best.insert(
                        key,
                        WalkingTransferMetrics {
                            duration_seconds: duration.0,
                            distance_metres: distance.0,
                        },
                    );
                }
            }
        }
    }

    let mut by_source =
        HashMap::<BoardingPointId, Vec<(BoardingPointId, WalkingTransferMetrics)>>::new();
    for ((from, to), metrics) in best {
        by_source.entry(from).or_default().push((to, metrics));
    }
    network.transfer_edges.clear();
    for (from, mut targets) in by_source {
        targets.sort_by(|a, b| {
            a.1.duration_seconds
                .cmp(&b.1.duration_seconds)
                .then_with(|| a.0.as_str().cmp(b.0.as_str()))
        });
        stats.removed_by_fanout += targets.len().saturating_sub(MAX_OUTGOING);
        for (to, metrics) in targets.into_iter().take(MAX_OUTGOING) {
            network.transfer_edges.push(TransferEdge::new(
                from.clone(),
                to,
                metrics.duration_seconds,
                metrics.distance_metres,
            ));
        }
    }
    network.transfer_edges.sort_by(|a, b| {
        a.from
            .as_str()
            .cmp(b.from.as_str())
            .then_with(|| a.to.as_str().cmp(b.to.as_str()))
    });
    stats.retained_edges = network.transfer_edges.len();
    let route_modes = network
        .routes
        .iter()
        .map(|route| (&route.id, route.mode))
        .collect::<HashMap<_, _>>();
    let modes = network
        .patterns
        .iter()
        .flat_map(|pattern| {
            let mode = route_modes
                .get(&pattern.route_id)
                .copied()
                .expect("validated pattern route");
            pattern.stops.iter().map(move |id| (id, mode))
        })
        .fold(HashMap::<_, HashSet<_>>::new(), |mut modes, (id, mode)| {
            modes.entry(id).or_default().insert(mode);
            modes
        });
    for edge in &network.transfer_edges {
        if let (Some(from), Some(to)) = (modes.get(&edge.from), modes.get(&edge.to)) {
            for &a in from {
                for &b in to {
                    *stats
                        .by_mode
                        .entry(format!("{} → {}", mode_name(a), mode_name(b)))
                        .or_default() += 1;
                }
            }
        }
    }
    stats.elapsed_milliseconds = started.elapsed().as_millis();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{
            ids::TransitPatternId,
            location::RequestCoordinate,
            transit::{TransitPattern, TransitStop, default_service_day_boundary},
        },
        routing::transfers::TransferShortcutIndex,
        street::{FakeStreetRouter, Seconds},
    };

    fn point(id: &str, latitude: f64) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(id),
            id,
            RequestCoordinate {
                latitude,
                longitude: 121.5,
            },
        )
    }

    fn network(stops: Vec<TransitStop>) -> TransitNetwork {
        let pattern = stops.iter().map(|stop| stop.id.clone()).collect();
        TransitNetwork::fixture(
            default_service_day_boundary(),
            stops,
            vec![TransitPattern::fixture(
                TransitPatternId::new("pattern"),
                TransitMode::Bus,
                "bus",
                "end",
                pattern,
            )],
            Vec::new(),
        )
    }

    fn coord(latitude: f64) -> Coordinate {
        Coordinate::try_new(latitude, 121.5).unwrap()
    }

    #[tokio::test]
    async fn walking_transfer_is_directional_and_rejects_unreachable_and_distant_stops() {
        let mut network = network(vec![
            point("a", 25.0),
            point("b", 25.0005),
            point("c", 25.001),
            point("far", 25.01),
        ]);
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                coord(25.0),
                coord(25.0005),
                Seconds(75),
                Metres(90),
            )
            .unreachable_leg(StreetTravelMode::Walking, coord(25.0), coord(25.001))
            .matrix_leg(
                StreetTravelMode::Walking,
                coord(25.0),
                coord(25.01),
                Seconds(20),
                Metres(20),
            )
            .build();
        let stats = enrich_network_with_static_transfers(&mut network, &street)
            .await
            .unwrap();
        assert_eq!(stats.boarding_points, 4);
        assert_eq!(stats.geographic_candidate_pairs, 6);
        assert_eq!(stats.matrix_pairs_queried, 6);
        assert_eq!(
            network.transfer_edges,
            vec![TransferEdge::new(
                BoardingPointId::new("a"),
                BoardingPointId::new("b"),
                75,
                90
            )]
        );
        let index = TransferShortcutIndex::from_network(&network);
        assert_eq!(index.transfers_from(&BoardingPointId::new("a")).len(), 1);
        assert_eq!(index.transfers_to(&BoardingPointId::new("b")).len(), 1);
        assert_eq!(
            index.transfers_from(&BoardingPointId::new("a"))[0].distance_metres,
            90
        );
        assert_eq!(
            index.transfers_to(&BoardingPointId::new("b"))[0].distance_metres,
            90
        );
        network.static_network_revision = "fixture".to_owned();
        network.schedule_revision = "fixture".to_owned();
        let restored =
            TransitNetwork::from_artifact_json(&serde_json::to_string(&network).unwrap()).unwrap();
        assert_eq!(restored.transfer_edges[0].distance_metres, 90);
        let mut legacy = serde_json::to_value(&network).unwrap();
        legacy["transfer_edges"][0]
            .as_object_mut()
            .unwrap()
            .remove("distance_metres");
        assert!(TransitNetwork::from_artifact_json(&legacy.to_string()).is_err());
        assert!(index.transfers_from(&BoardingPointId::new("b")).is_empty());
    }

    #[tokio::test]
    async fn duration_and_distance_limits_apply_to_via_generated_transfers() {
        let mut network = network(vec![
            point("a", 25.0),
            point("b", 25.0005),
            point("c", 25.001),
        ]);
        network.transfer_edges = vec![
            TransferEdge::new(
                BoardingPointId::new("a"),
                BoardingPointId::new("b"),
                150,
                100,
            ),
            TransferEdge::new(
                BoardingPointId::new("a"),
                BoardingPointId::new("b"),
                180,
                100,
            ),
            TransferEdge::new(
                BoardingPointId::new("b"),
                BoardingPointId::new("a"),
                60,
                100,
            ),
        ];
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                coord(25.0),
                coord(25.0005),
                Seconds(75),
                Metres(90),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                coord(25.0005),
                coord(25.0),
                Seconds(200),
                Metres(90),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                coord(25.0),
                coord(25.001),
                Seconds(481),
                Metres(120),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                coord(25.001),
                coord(25.0),
                Seconds(100),
                Metres(801),
            )
            .build();
        let stats = enrich_network_with_static_transfers(&mut network, &street)
            .await
            .unwrap();
        assert_eq!(
            network.transfer_edges,
            vec![
                TransferEdge::new(BoardingPointId::new("a"), BoardingPointId::new("b"), 75, 90),
                TransferEdge::new(
                    BoardingPointId::new("b"),
                    BoardingPointId::new("a"),
                    200,
                    90
                ),
            ]
        );
        assert_eq!(stats.removed_by_limits, 1);
    }

    #[tokio::test]
    async fn outgoing_fanout_keeps_shortest_twelve() {
        let mut stops = vec![point("origin", 25.0)];
        let mut builder = FakeStreetRouter::builder();
        for index in 1..=14 {
            let latitude = 25.0 + f64::from(index) * 0.0001;
            stops.push(point(&format!("target{index:02}"), latitude));
            builder = builder.matrix_leg(
                StreetTravelMode::Walking,
                coord(25.0),
                coord(latitude),
                Seconds(index * 10),
                Metres(index * 12),
            );
        }
        let mut network = network(stops);
        let stats = enrich_network_with_static_transfers(&mut network, &builder.build())
            .await
            .unwrap();
        assert_eq!(
            network
                .transfer_edges
                .iter()
                .filter(|edge| edge.from.as_str() == "origin")
                .count(),
            12
        );
        assert!(
            !network
                .transfer_edges
                .iter()
                .any(|edge| edge.from.as_str() == "origin" && edge.to.as_str() == "target14")
        );
        assert_eq!(stats.removed_by_fanout, 2);
    }

    #[tokio::test]
    async fn reachable_walking_target_is_not_lost_in_a_dense_geographic_cluster() {
        let mut stops = vec![point("origin", 25.0)];
        for index in 1..=65 {
            stops.push(point(
                &format!("target{index:02}"),
                25.0 + f64::from(index) * 0.00001,
            ));
        }
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                coord(25.0),
                coord(25.00065),
                Seconds(40),
                Metres(80),
            )
            .build();
        let mut network = network(stops);
        let stats = enrich_network_with_static_transfers(&mut network, &street)
            .await
            .unwrap();
        assert!(stats.matrix_pairs_queried >= 65);
        assert_eq!(network.transfer_edges.len(), 1);
        assert_eq!(network.transfer_edges[0].to.as_str(), "target65");
    }
}
