use std::sync::Arc;

use geo::{Destination, HaversineMeasure, Point};
use via_server::domain::{
    ids::BoardingPointId,
    location::GeoCoordinate,
    transit::{TransitNetwork, TransitStop},
    transit_runtime::TransitRuntime,
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

fn brute_force_nearest(
    stops: &[TransitStop],
    origin: GeoCoordinate,
    limit: usize,
) -> Vec<&TransitStop> {
    brute_force_sorted(stops, origin)
        .into_iter()
        .take(limit)
        .map(|(stop, _)| stop)
        .collect()
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
fn spatial_candidate_selection_is_distance_ordered_not_artifact_ordered() {
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).expect("valid fixture time"),
        vec![
            stop("far", 25.20, 121.70),
            stop("near", 25.001, 121.501),
            stop("middle", 25.05, 121.55),
        ],
        Vec::new(),
        Vec::new(),
    );

    let runtime = TransitRuntime::new(Arc::new(network));
    let nearest = runtime.nearest_stops(
        GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
        2,
    );
    assert_eq!(nearest[0].id.as_str(), "near");
    assert_eq!(nearest[1].id.as_str(), "middle");
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
        for limit in [0, 1, 7, 75, 1_200] {
            assert_eq!(
                ids(runtime.nearest_stops(origin, limit)),
                ids(brute_force_nearest(&stops, origin, limit)),
                "nearest mismatch at {origin:?}, limit={limit}"
            );
        }
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
    assert!(empty.nearest_stops(origin, 5).is_empty());
    assert!(empty.nearest_stops(origin, 0).is_empty());
    assert!(empty.stops_within_radius(origin, 1_000).is_empty());

    let stops = vec![
        stop("same-z", 25.0, 121.5),
        stop("near-east", 25.0, 121.50001),
        stop("same-a", 25.0, 121.5),
        stop("near-north", 25.00001, 121.5),
    ];
    let runtime = runtime(stops);
    assert_eq!(
        ids(runtime.nearest_stops(origin, 1)),
        expected_ids(&["same-a"])
    );
    assert_eq!(
        ids(runtime.stops_within_radius(origin, 0)),
        expected_ids(&["same-a", "same-z"])
    );
    assert_eq!(
        ids(runtime.nearest_stops(origin, 4)),
        expected_ids(&["same-a", "same-z", "near-east", "near-north"])
    );
    assert_eq!(ids(runtime.nearest_stops(origin, 0)), Vec::<String>::new());
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
    let nearest = scale_runtime.nearest_stops(high_latitude_origin, 1);
    assert_eq!(nearest[0].id.as_str(), "longitude-degree");

    let dateline_origin = coordinate(0.0, 179.999);
    let dateline_runtime = runtime(vec![
        stop("across-dateline", 0.0, -179.999),
        stop("same-side", 0.0, 179.99),
    ]);
    assert_eq!(
        dateline_runtime.nearest_stops(dateline_origin, 1)[0]
            .id
            .as_str(),
        "across-dateline"
    );
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
        polar_runtime.nearest_stops(polar_origin, 1)[0].id.as_str(),
        "polar-near"
    );
    assert_eq!(
        ids(polar_runtime.stops_within_radius(polar_origin, 300)),
        expected_ids(&["polar-near"])
    );
}
