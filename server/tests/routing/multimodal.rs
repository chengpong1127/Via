use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use time::OffsetDateTime;
use via_server::{
    domain::{
        bikeshare::{
            BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
            BikeStationAvailability, BikeWalkingAccess,
        },
        ids::BikeStationId,
    },
    providers::{BikeAvailabilityError, BikeAvailabilitySource, UnavailableBikeAvailabilitySource},
    routing::BikeShortcut,
    street::{FakeStreetRouter, StreetTravelMode},
};

use super::fixtures::{
    FixtureRealtime, leg_modes, near_stop, network, option_for_patterns, request,
    route_body_with_bike, transit_street,
};
use crate::support::routing::{path, stop_coordinate};

#[derive(Clone)]
struct FixtureAvailabilitySource {
    snapshot: Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BikeAvailabilitySource for FixtureAvailabilitySource {
    async fn snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.snapshot.clone()
    }
}

fn bike_snapshot(ids: &[(&str, u32, u32)]) -> Arc<BikeAvailabilitySnapshot> {
    let observed_at = OffsetDateTime::now_utc();
    let stations = ids
        .iter()
        .map(|(id, bikes, slots)| {
            let id = BikeStationId::new(*id);
            (
                id.clone(),
                BikeStationAvailability::new(id, *bikes, *slots, true, observed_at),
            )
        })
        .collect::<HashMap<_, _>>();
    Arc::new(BikeAvailabilitySnapshot::new(observed_at, stations))
}

fn segment_network(
    from: &str,
    to: &str,
    pickup: via_server::street::Coordinate,
    dropoff: via_server::street::Coordinate,
) -> (
    Arc<BikeShareNetwork>,
    via_server::street::Coordinate,
    via_server::street::Coordinate,
) {
    let pickup_id = BikeStationId::new("pickup");
    let dropoff_id = BikeStationId::new("dropoff");
    let network = BikeShareNetwork::new(vec![
        BikeStation::new(
            pickup_id.clone(),
            "Fixture Pickup",
            via_server::api::RequestCoordinate {
                latitude: pickup.latitude(),
                longitude: pickup.longitude(),
            },
        ),
        BikeStation::new(
            dropoff_id.clone(),
            "Fixture Dropoff",
            via_server::api::RequestCoordinate {
                latitude: dropoff.latitude(),
                longitude: dropoff.longitude(),
            },
        ),
    ])
    .with_access_links(vec![
        BikeStationAccess::new(
            via_server::domain::ids::BoardingPointId::new(from),
            pickup_id,
            Some(BikeWalkingAccess::new(10, 50)),
            None,
        ),
        BikeStationAccess::new(
            via_server::domain::ids::BoardingPointId::new(to),
            dropoff_id,
            None,
            Some(BikeWalkingAccess::new(10, 50)),
        ),
    ])
    .with_revision("fixture-bike");
    (Arc::new(network), pickup, dropoff)
}

#[tokio::test]
async fn bike_can_replace_a_transit_leg() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let m1 = stop_coordinate(&transit, "F-M1");
    let m2 = stop_coordinate(&transit, "F-M2-RED");
    let (bike_network, pickup, dropoff) = segment_network("F-M1", "F-M2-RED", m1, m2);
    let street = FakeStreetRouter::builder()
        .path(StreetTravelMode::Walking, origin, m1, path(origin, m1, 60))
        .path(
            StreetTravelMode::Walking,
            m2,
            destination,
            path(m2, destination, 60),
        )
        .path(StreetTravelMode::Walking, m1, pickup, path(m1, pickup, 10))
        .path(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            path(pickup, dropoff, 90),
        )
        .path(
            StreetTravelMode::Walking,
            dropoff,
            m2,
            path(dropoff, m2, 10),
        )
        .build();
    let calls = Arc::new(AtomicUsize::new(0));
    let body = route_body_with_bike(
        street,
        FixtureRealtime::empty(),
        request(origin, destination, &["metro", "bike_share"]),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(&[("pickup", 2, 0), ("dropoff", 0, 2)])),
            calls: calls.clone(),
        }),
    )
    .await;

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let options = body["journey_options"].as_array().unwrap();
    assert!(
        options
            .iter()
            .any(|option| leg_modes(option).contains(&"transit"))
    );
    assert!(options.iter().any(|option| option["bike_share_uses"] == 1));
}

#[tokio::test]
async fn bike_availability_is_not_loaded_when_bike_share_is_not_selected() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let calls = Arc::new(AtomicUsize::new(0));
    let _ = route_body_with_bike(
        transit_street(&transit, origin, "F-M1", destination, "F-M2-RED"),
        FixtureRealtime::empty(),
        request(origin, destination, &["metro"]),
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(&[])),
            calls: calls.clone(),
        }),
    )
    .await;
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn slower_bike_segment_is_rejected() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let m1 = stop_coordinate(&transit, "F-M1");
    let m2 = stop_coordinate(&transit, "F-M2-RED");
    let (bike_network, pickup, dropoff) = segment_network("F-M1", "F-M2-RED", m1, m2);
    let street = FakeStreetRouter::builder()
        .path(StreetTravelMode::Walking, origin, m1, path(origin, m1, 60))
        .path(
            StreetTravelMode::Walking,
            m2,
            destination,
            path(m2, destination, 60),
        )
        .path(StreetTravelMode::Walking, m1, pickup, path(m1, pickup, 10))
        .path(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            path(pickup, dropoff, 400),
        )
        .path(
            StreetTravelMode::Walking,
            dropoff,
            m2,
            path(dropoff, m2, 10),
        )
        .build();
    let body = route_body_with_bike(
        street,
        FixtureRealtime::empty(),
        request(origin, destination, &["metro", "bike_share"]),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(&[("pickup", 2, 0), ("dropoff", 0, 2)])),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .await;
    assert!(
        body["journey_options"]
            .as_array()
            .unwrap()
            .iter()
            .all(|option| option["bike_share_uses"] == 0)
    );
}

async fn surface_transfer_options(bikes: u32, docks: u32) -> serde_json::Value {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M3-BLUE");
    let red = stop_coordinate(&transit, "F-M2-RED");
    let blue = stop_coordinate(&transit, "F-M2-BLUE");
    let (bike_network, pickup, dropoff) = segment_network("F-M2-RED", "F-M2-BLUE", red, blue);
    let m1 = stop_coordinate(&transit, "F-M1");
    let m3 = stop_coordinate(&transit, "F-M3-BLUE");
    let street = FakeStreetRouter::builder()
        .path(StreetTravelMode::Walking, origin, m1, path(origin, m1, 60))
        .path(
            StreetTravelMode::Walking,
            m3,
            destination,
            path(m3, destination, 60),
        )
        .path(
            StreetTravelMode::Walking,
            red,
            pickup,
            path(red, pickup, 10),
        )
        .path(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            path(pickup, dropoff, 60),
        )
        .path(
            StreetTravelMode::Walking,
            dropoff,
            blue,
            path(dropoff, blue, 10),
        )
        .build();
    route_body_with_bike(
        street,
        FixtureRealtime::empty(),
        request(
            origin,
            destination,
            &["bus", "metro", "tra", "thsr", "bike_share"],
        ),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(&[
                ("pickup", bikes, 0),
                ("dropoff", 0, docks),
            ])),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .await
}

#[tokio::test]
async fn bike_can_realize_a_surface_transfer_without_skipping_transit() {
    let body = surface_transfer_options(2, 2).await;
    let options = body["journey_options"].as_array().unwrap();
    assert!(
        options.iter().any(|option| {
            option["bike_share_uses"] == 0
                && option["legs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|leg| leg["details"]["via_pattern_id"].as_str())
                    .eq(["fixture-mrt-red", "fixture-mrt-blue"])
        }),
        "walking realization missing: {body}"
    );
    let bike = options
        .iter()
        .find(|option| option["bike_share_uses"] == 1)
        .unwrap_or_else(|| panic!("bike transfer option missing: {body}"));
    assert_eq!(bike["transfer_count"], 1);
    assert_eq!(bike["walking_seconds"], 140);
    assert_eq!(
        bike["legs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|leg| leg["details"]["via_pattern_id"].as_str())
            .collect::<Vec<_>>(),
        vec!["fixture-mrt-red", "fixture-mrt-blue"]
    );
}

#[tokio::test]
async fn no_bike_at_pickup_keeps_the_walking_transfer() {
    let body = surface_transfer_options(0, 2).await;
    let options = body["journey_options"].as_array().unwrap();
    assert!(options.iter().any(|option| option["bike_share_uses"] == 0));
    assert!(options.iter().all(|option| option["bike_share_uses"] == 0));
}

#[tokio::test]
async fn no_dock_at_dropoff_keeps_the_walking_transfer() {
    let body = surface_transfer_options(2, 0).await;
    let options = body["journey_options"].as_array().unwrap();
    assert!(options.iter().any(|option| option["bike_share_uses"] == 0));
    assert!(options.iter().all(|option| option["bike_share_uses"] == 0));
}

#[tokio::test]
async fn bike_availability_failure_keeps_the_transit_baseline() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let body = route_body_with_bike(
        transit_street(&transit, origin, "F-M1", destination, "F-M2-RED"),
        FixtureRealtime::empty(),
        request(origin, destination, &["metro", "bike_share"]),
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
        Arc::new(UnavailableBikeAvailabilitySource),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-mrt-red"]);
    assert_eq!(leg_modes(option), vec!["walking", "transit", "walking"]);
}

#[tokio::test]
async fn direct_walking_gains_a_bike_share_variant_with_the_shared_snapshot() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let pickup = via_server::street::Coordinate::try_new(
        origin.latitude() + 0.0001,
        origin.longitude() + 0.0001,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        destination.latitude() - 0.0001,
        destination.longitude() - 0.0001,
    )
    .unwrap();
    let pickup_id = BikeStationId::new("direct-pickup");
    let dropoff_id = BikeStationId::new("direct-dropoff");
    let bike_network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(
                pickup_id,
                "Direct pickup",
                via_server::api::RequestCoordinate {
                    latitude: pickup.latitude(),
                    longitude: pickup.longitude(),
                },
            ),
            BikeStation::new(
                dropoff_id,
                "Direct dropoff",
                via_server::api::RequestCoordinate {
                    latitude: dropoff.latitude(),
                    longitude: dropoff.longitude(),
                },
            ),
        ])
        .with_revision("fixture-direct-bike"),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let body = route_body_with_bike(
        FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                stop_coordinate(&transit, "F-M1"),
                path(origin, stop_coordinate(&transit, "F-M1"), 60),
            )
            .path(
                StreetTravelMode::Walking,
                stop_coordinate(&transit, "F-M2-RED"),
                destination,
                path(stop_coordinate(&transit, "F-M2-RED"), destination, 60),
            )
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                path(origin, destination, 900),
            )
            .path(
                StreetTravelMode::Walking,
                origin,
                pickup,
                path(origin, pickup, 30),
            )
            .path(
                StreetTravelMode::Bicycle,
                pickup,
                dropoff,
                path(pickup, dropoff, 300),
            )
            .path(
                StreetTravelMode::Walking,
                dropoff,
                destination,
                path(dropoff, destination, 30),
            )
            .build(),
        FixtureRealtime::empty(),
        request(origin, destination, &["metro", "bike_share"]),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(&[
                ("direct-pickup", 2, 0),
                ("direct-dropoff", 0, 2),
            ])),
            calls: calls.clone(),
        }),
    )
    .await;
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(options.iter().any(|option| option["bike_share_uses"] == 1));
}

#[tokio::test]
async fn direct_walking_without_a_transit_query_can_use_bike_share() {
    let origin = via_server::street::Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = via_server::street::Coordinate::try_new(25.01, 121.51).unwrap();
    let pickup = via_server::street::Coordinate::try_new(25.0001, 121.5001).unwrap();
    let dropoff = via_server::street::Coordinate::try_new(25.0099, 121.5099).unwrap();
    let pickup_id = BikeStationId::new("direct-pickup");
    let dropoff_id = BikeStationId::new("direct-dropoff");
    let bike_network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(
                pickup_id,
                "Direct pickup",
                via_server::api::RequestCoordinate {
                    latitude: pickup.latitude(),
                    longitude: pickup.longitude(),
                },
            ),
            BikeStation::new(
                dropoff_id,
                "Direct dropoff",
                via_server::api::RequestCoordinate {
                    latitude: dropoff.latitude(),
                    longitude: dropoff.longitude(),
                },
            ),
        ])
        .with_revision("fixture-direct-bike"),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let body = route_body_with_bike(
        FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                path(origin, destination, 900),
            )
            .path(
                StreetTravelMode::Walking,
                origin,
                pickup,
                path(origin, pickup, 30),
            )
            .path(
                StreetTravelMode::Bicycle,
                pickup,
                dropoff,
                path(pickup, dropoff, 300),
            )
            .path(
                StreetTravelMode::Walking,
                dropoff,
                destination,
                path(dropoff, destination, 30),
            )
            .build(),
        FixtureRealtime::empty(),
        request(origin, destination, &["bike_share"]),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(&[
                ("direct-pickup", 2, 0),
                ("direct-dropoff", 0, 2),
            ])),
            calls: calls.clone(),
        }),
    )
    .await;

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(
        body["journey_options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["bike_share_uses"] == 1)
    );
}

#[test]
fn bike_shortcut_can_span_multiple_forward_anchors() {
    assert!(
        BikeShortcut::new(
            1,
            3,
            BikeStationId::new("pickup"),
            BikeStationId::new("dropoff"),
            BikeWalkingAccess::new(1, 1),
            via_server::routing::BikeRide {
                duration_seconds: 1,
                distance_metres: 1,
            },
            BikeWalkingAccess::new(1, 1),
        )
        .is_some()
    );
    assert!(
        BikeShortcut::new(
            3,
            1,
            BikeStationId::new("pickup"),
            BikeStationId::new("dropoff"),
            BikeWalkingAccess::new(1, 1),
            via_server::routing::BikeRide {
                duration_seconds: 1,
                distance_metres: 1,
            },
            BikeWalkingAccess::new(1, 1),
        )
        .is_none()
    );
}
