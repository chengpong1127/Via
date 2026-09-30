use std::sync::Arc;

use via_server::domain::{
    ids::BoardingPointId,
    location::RequestCoordinate,
    transit::{TransitNetwork, TransitStop},
    transit_runtime::TransitRuntime,
};

fn stop(id: &str, latitude: f64, longitude: f64) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        id,
        RequestCoordinate {
            latitude,
            longitude,
        },
    )
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
        RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
        2,
    );
    assert_eq!(nearest[0].id.as_str(), "near");
    assert_eq!(nearest[1].id.as_str(), "middle");
}
