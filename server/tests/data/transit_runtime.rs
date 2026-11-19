use std::sync::Arc;

use geo::{Destination, HaversineMeasure, Point};
use via_server::domain::{
    ids::{BoardingPointId, TransitPatternId, TransitRouteId},
    location::GeoCoordinate,
    transit::network::{SegmentProfile, TransferEdge, TransitNetwork, TransitPattern, TransitStop},
    transit::runtime::{PatternPosition, TransitRuntime},
};

fn stop(id: &str, latitude: f64, longitude: f64) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        id,
        GeoCoordinate {
            latitude,
            longitude,
        },
    )
}

fn coordinate(latitude: f64, longitude: f64) -> GeoCoordinate {
    GeoCoordinate {
        latitude,
        longitude,
    }
}

fn runtime(stops: Vec<TransitStop>) -> TransitRuntime {
    TransitRuntime::new(Arc::new(TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).expect("valid fixture time"),
        stops,
        Vec::new(),
        Vec::new(),
    )))
}

fn brute_force_sorted(stops: &[TransitStop], origin: GeoCoordinate) -> Vec<(&TransitStop, f64)> {
    let mut candidates = stops
        .iter()
        .map(|stop| (stop, origin.distance_metres(stop.coordinate)))
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, left_distance), (right, right_distance)| {
        left_distance
            .total_cmp(right_distance)
            .then(left.id.as_str().cmp(right.id.as_str()))
    });
    candidates
}

fn brute_force_radius(
    stops: &[TransitStop],
    origin: GeoCoordinate,
    radius_metres: u32,
) -> Vec<&TransitStop> {
    brute_force_sorted(stops, origin)
        .into_iter()
        .filter(|(_, distance)| *distance <= f64::from(radius_metres))
        .map(|(stop, _)| stop)
        .collect()
}

fn ids(stops: Vec<&TransitStop>) -> Vec<String> {
    stops
        .into_iter()
        .map(|stop| stop.id.as_str().to_owned())
        .collect()
}

fn expected_ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn spatial_queries_match_brute_force_geographic_results_for_taiwan_fixture() {
    let mut state = 0x5eed_u64;
    let stops = (0..1_000)
        .map(|index| {
            // Deterministic pseudo-distribution across Taiwan and nearby waters.
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let latitude_fraction = (state >> 11) as f64 / ((1_u64 << 53) as f64);
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let longitude_fraction = (state >> 11) as f64 / ((1_u64 << 53) as f64);
            stop(
                &format!("stop-{index:04}"),
                21.8 + latitude_fraction * 4.3,
                119.2 + longitude_fraction * 3.0,
            )
        })
        .collect::<Vec<_>>();
    let runtime = runtime(stops.clone());
    let origins = [
        coordinate(25.033, 121.565),
        coordinate(22.627, 120.301),
        coordinate(24.8, 120.9),
        coordinate(21.95, 120.78),
    ];

    for origin in origins {
        for radius in [0, 100, 1_000, 5_000, 25_000, 100_000] {
            assert_eq!(
                ids(runtime.stops_within_radius(origin, radius)),
                ids(brute_force_radius(&stops, origin, radius)),
                "radius mismatch at {origin:?}, radius={radius}m"
            );
        }
    }
}

#[test]
fn spatial_queries_handle_empty_duplicate_and_nearby_points_deterministically() {
    let empty = runtime(Vec::new());
    let origin = coordinate(25.0, 121.5);
    assert!(empty.stops_within_radius(origin, 1_000).is_empty());

    let stops = vec![
        stop("same-z", 25.0, 121.5),
        stop("near-east", 25.0, 121.50001),
        stop("same-a", 25.0, 121.5),
        stop("near-north", 25.00001, 121.5),
    ];
    let runtime = runtime(stops);
    assert_eq!(
        ids(runtime.stops_within_radius(origin, 0)),
        expected_ids(&["same-a", "same-z"])
    );
    assert_eq!(
        ids(runtime.stops_within_radius(origin, 2)),
        expected_ids(&["same-a", "same-z", "near-east", "near-north"])
    );
}

#[test]
fn spatial_radius_includes_points_only_within_the_geographic_radius() {
    let origin = coordinate(25.0, 121.5);
    let destination = HaversineMeasure::new(6_371_000.0).destination(
        Point::new(origin.longitude, origin.latitude),
        90.0,
        99.9,
    );
    let boundary_coordinate = coordinate(destination.y(), destination.x());
    let runtime = runtime(vec![stop(
        "boundary",
        boundary_coordinate.latitude,
        boundary_coordinate.longitude,
    )]);
    assert!((origin.distance_metres(boundary_coordinate) - 99.9).abs() < 1e-8);
    assert_eq!(
        ids(runtime.stops_within_radius(origin, 100)),
        expected_ids(&["boundary"])
    );
    assert!(runtime.stops_within_radius(origin, 99).is_empty());
}

#[test]
fn spatial_queries_use_geographic_scale_across_longitude_dateline_and_poles() {
    // At 60°N, a longitude degree is roughly half a latitude degree in distance.
    let high_latitude_origin = coordinate(60.0, 0.0);
    let scale_runtime = runtime(vec![
        stop("longitude-degree", 60.0, 1.0),
        stop("latitude-half-degree", 60.5, 0.0),
    ]);
    assert_eq!(
        ids(scale_runtime.stops_within_radius(high_latitude_origin, 56_000)),
        expected_ids(&["longitude-degree", "latitude-half-degree"])
    );

    let dateline_origin = coordinate(0.0, 179.999);
    let dateline_runtime = runtime(vec![
        stop("across-dateline", 0.0, -179.999),
        stop("same-side", 0.0, 179.99),
    ]);
    assert_eq!(
        ids(dateline_runtime.stops_within_radius(dateline_origin, 300)),
        expected_ids(&["across-dateline"])
    );

    let polar_origin = coordinate(89.999, 40.0);
    let polar_runtime = runtime(vec![
        stop("polar-near", 89.999, -140.0),
        stop("polar-far", 89.99, 40.0),
    ]);
    assert_eq!(
        ids(polar_runtime.stops_within_radius(polar_origin, 300)),
        expected_ids(&["polar-near"])
    );
}

fn pattern(id: &str, stops: &[&str]) -> TransitPattern {
    TransitPattern::new(
        TransitPatternId::new(id),
        TransitRouteId::new(id),
        None,
        id,
        stops
            .iter()
            .map(|stop| BoardingPointId::new(*stop))
            .collect(),
        SegmentProfile {
            segments: Vec::new(),
        },
    )
}

#[test]
fn pattern_positions_list_every_pattern_occurrence_in_pattern_order() {
    let runtime = TransitRuntime::new(Arc::new(TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).expect("valid fixture time"),
        vec![stop("a", 25.0, 121.5), stop("b", 25.0, 121.5)],
        vec![pattern("z", &["a", "b", "a"]), pattern("m", &["b", "a"])],
        Vec::new(),
    )));

    assert_eq!(
        runtime.pattern_positions_at(&BoardingPointId::new("a")),
        &[
            PatternPosition {
                pattern_id: TransitPatternId::new("m"),
                stop_index: 1,
            },
            PatternPosition {
                pattern_id: TransitPatternId::new("z"),
                stop_index: 0,
            },
            PatternPosition {
                pattern_id: TransitPatternId::new("z"),
                stop_index: 2,
            },
        ]
    );
    assert!(
        runtime
            .pattern_positions_at(&BoardingPointId::new("missing"))
            .is_empty()
    );
}

#[test]
fn transfers_are_directional_deduplicated_and_ordered() {
    let edge = |from: &str, to: &str, duration_seconds| {
        TransferEdge::new(
            BoardingPointId::new(from),
            BoardingPointId::new(to),
            duration_seconds,
            100,
        )
    };
    let mut network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).expect("valid fixture time"),
        vec![
            stop("a", 25.0, 121.5),
            stop("b", 25.0, 121.5),
            stop("c", 25.0, 121.5),
        ],
        Vec::new(),
        Vec::new(),
    );
    network.transfer_edges = vec![
        edge("a", "c", 60),
        edge("a", "b", 90),
        edge("a", "b", 90),
        edge("a", "b", 30),
        edge("b", "a", 90),
    ];
    let runtime = TransitRuntime::new(Arc::new(network));

    assert_eq!(
        runtime.transfers_from(&BoardingPointId::new("a")),
        &[edge("a", "b", 30), edge("a", "b", 90), edge("a", "c", 60)]
    );
    assert_eq!(
        runtime.transfers_from(&BoardingPointId::new("b")),
        &[edge("b", "a", 90)]
    );
    assert!(
        runtime
            .transfers_from(&BoardingPointId::new("c"))
            .is_empty()
    );
}
