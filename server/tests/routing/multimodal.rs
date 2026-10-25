use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use serde_json::Value;
use time::{OffsetDateTime, macros::date};
use via_server::{
    domain::{
        bikeshare::{
            BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
            BikeStationAvailability, BikeWalkingAccess,
        },
        ids::BikeStationId,
        transit_runtime::TransitRuntime,
        transit_timing::{EffectiveTrip, TimingSnapshot},
    },
    providers::{BikeAvailabilityError, BikeAvailabilitySource, UnavailableBikeAvailabilitySource},
    street::{FakeStreetRouter, StreetTravelMode},
};

use super::fixtures::{
    CountingStreet, CountingTiming, FixtureRealtime, leg_modes, near_stop, network,
    option_for_patterns, request, route_body_with_bike, route_body_with_sources, transit_street,
};
use crate::support::routing::{path, stop_coordinate};

#[derive(Clone)]
struct FixtureAvailabilitySource {
    snapshot: Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BikeAvailabilitySource for FixtureAvailabilitySource {
    async fn snapshot(
        &self,
        _station_ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
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

fn bike_share_uses(option: &Value) -> usize {
    option["legs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|leg| leg["details"]["type"] == "bike_share")
        .count()
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
            via_server::api::GeoCoordinate {
                latitude: pickup.latitude(),
                longitude: pickup.longitude(),
            },
        ),
        BikeStation::new(
            dropoff_id.clone(),
            "Fixture Dropoff",
            via_server::api::GeoCoordinate {
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
    assert!(options.iter().any(|option| bike_share_uses(option) == 1));
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
async fn slower_bike_preserves_transit_and_can_reduce_direct_walking() {
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
    let options = body["journey_options"].as_array().unwrap();
    assert!(
        options
            .iter()
            .any(|option| bike_share_uses(option) == 0 && leg_modes(option).contains(&"transit"))
    );
    assert!(
        options
            .iter()
            .any(|option| bike_share_uses(option) == 1 && !leg_modes(option).contains(&"transit"))
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
            bike_share_uses(option) == 0
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
        .find(|option| bike_share_uses(option) == 1)
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
    assert!(options.iter().any(|option| bike_share_uses(option) == 0));
    assert!(options.iter().all(|option| bike_share_uses(option) == 0));
}

#[tokio::test]
async fn no_dock_at_dropoff_keeps_the_walking_transfer() {
    let body = surface_transfer_options(2, 0).await;
    let options = body["journey_options"].as_array().unwrap();
    assert!(options.iter().any(|option| bike_share_uses(option) == 0));
    assert!(options.iter().all(|option| bike_share_uses(option) == 0));
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
async fn viable_transit_corridor_competes_with_direct_bike_share() {
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
                via_server::api::GeoCoordinate {
                    latitude: pickup.latitude(),
                    longitude: pickup.longitude(),
                },
            ),
            BikeStation::new(
                dropoff_id,
                "Direct dropoff",
                via_server::api::GeoCoordinate {
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
    assert!(
        options
            .iter()
            .any(|option| leg_modes(option).contains(&"transit"))
    );
    assert!(
        options
            .iter()
            .any(|option| bike_share_uses(option) == 1 && !leg_modes(option).contains(&"transit"))
    );
    assert!(
        options
            .iter()
            .all(|option| leg_modes(option) != vec!["walking"])
    );
}

async fn trivial_corridor_bike_options(modes: &[&str]) {
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
                via_server::api::GeoCoordinate {
                    latitude: pickup.latitude(),
                    longitude: pickup.longitude(),
                },
            ),
            BikeStation::new(
                dropoff_id,
                "Direct dropoff",
                via_server::api::GeoCoordinate {
                    latitude: dropoff.latitude(),
                    longitude: dropoff.longitude(),
                },
            ),
        ])
        .with_revision("fixture-direct-bike"),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let timing = Arc::new(CountingTiming::default());
    let body = route_body_with_sources(
        Arc::new(
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
        ),
        timing.clone(),
        request(origin, destination, modes),
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
    assert_eq!(timing.calls.load(Ordering::Relaxed), 0);
    assert!(
        body["journey_options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| leg_modes(option) == vec!["walking"])
    );
    assert!(
        body["journey_options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| bike_share_uses(option) == 1)
    );
}

#[tokio::test]
async fn bike_only_uses_the_trivial_corridor_for_both_surface_modes() {
    trivial_corridor_bike_options(&["bike_share"]).await;
}

#[tokio::test]
async fn transit_without_reachable_corridors_uses_the_direct_bike_corridor() {
    trivial_corridor_bike_options(&["metro", "bike_share"]).await;
}

#[tokio::test]
async fn walking_only_skips_timing_and_bike_availability() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let timing = Arc::new(CountingTiming::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let body = route_body_with_sources(
        Arc::new(
            FakeStreetRouter::builder()
                .path(
                    StreetTravelMode::Walking,
                    origin,
                    destination,
                    path(origin, destination, 300),
                )
                .build(),
        ),
        timing.clone(),
        request(origin, destination, &[]),
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
        Arc::new(FixtureAvailabilitySource {
            snapshot: Err(BikeAvailabilityError::TemporarilyUnavailable),
            calls: calls.clone(),
        }),
    )
    .await;

    assert_eq!(timing.calls.load(Ordering::Relaxed), 0);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(leg_modes(&options[0]), vec!["walking"]);
    assert_eq!(options[0]["duration_seconds"], 300);
}

async fn bike_only_walking_result(
    availability: Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>,
) {
    let origin = crate::support::routing::coordinate(25.30, 121.60);
    let destination = crate::support::routing::coordinate(25.301, 121.601);
    let timing = Arc::new(CountingTiming::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let (bike_network, _, _) = segment_network("F-M1", "F-M2-RED", origin, destination);
    let body = route_body_with_sources(
        Arc::new(
            FakeStreetRouter::builder()
                .path(
                    StreetTravelMode::Walking,
                    origin,
                    destination,
                    path(origin, destination, 300),
                )
                .build(),
        ),
        timing.clone(),
        request(origin, destination, &["bike_share"]),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: availability,
            calls: calls.clone(),
        }),
    )
    .await;

    assert_eq!(timing.calls.load(Ordering::Relaxed), 0);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(leg_modes(&options[0]), vec!["walking"]);
    assert_eq!(options[0]["duration_seconds"], 300);
}

#[tokio::test]
async fn bike_only_keeps_walking_when_pickup_has_no_bikes() {
    bike_only_walking_result(Ok(bike_snapshot(&[("pickup", 0, 0), ("dropoff", 0, 2)]))).await;
}

#[tokio::test]
async fn bike_only_keeps_walking_when_availability_fails() {
    bike_only_walking_result(Err(BikeAvailabilityError::TemporarilyUnavailable)).await;
}

#[tokio::test]
async fn reconstruction_failure_preserves_direct_walking_without_reloading_snapshots() {
    for max_candidates in [1, 16] {
        let transit = network();
        let origin = near_stop(&transit, "F-M1");
        let destination = near_stop(&transit, "F-M2-RED");
        let access_stop = stop_coordinate(&transit, "F-M1");
        let egress_stop = stop_coordinate(&transit, "F-M2-RED");
        let runtime = TransitRuntime::new(Arc::new(transit.clone()));
        let mut snapshot = TimingSnapshot::default();
        for scheduled in transit
            .trips
            .iter()
            .filter(|trip| trip.pattern_id.as_str() == "fixture-mrt-red")
        {
            let trip =
                EffectiveTrip::from_scheduled(&runtime, date!(2026 - 08 - 31), &scheduled.id)
                    .unwrap();
            snapshot.insert(trip.pattern_id.clone(), 0, vec![trip]);
        }
        let timing = Arc::new(CountingTiming {
            calls: AtomicUsize::new(0),
            snapshot,
        });
        let availability_calls = Arc::new(AtomicUsize::new(0));
        let street = Arc::new(CountingStreet {
            inner: FakeStreetRouter::builder()
                .path(
                    StreetTravelMode::Walking,
                    origin,
                    access_stop,
                    path(origin, access_stop, 60),
                )
                .path(
                    StreetTravelMode::Walking,
                    egress_stop,
                    destination,
                    path(egress_stop, destination, 60),
                )
                .path(
                    StreetTravelMode::Walking,
                    origin,
                    destination,
                    path(origin, destination, 3000),
                )
                .build(),
            origin,
            destination,
            direct_calls: AtomicUsize::new(0),
            route_failure_at: Some((origin, access_stop)),
        });
        let mut limits = via_server::routing::JourneyPlanningLimits::default();
        limits.planner.max_candidates = max_candidates;
        let service = via_server::routing::RoutingService::new(
            street.clone(),
            Arc::new(TransitRuntime::new(Arc::new(transit))),
            timing.clone(),
            Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
            Arc::new(FixtureAvailabilitySource {
                snapshot: Ok(bike_snapshot(&[])),
                calls: availability_calls.clone(),
            }),
            limits,
            false,
        );
        let request =
            serde_json::from_value(request(origin, destination, &["metro", "bike_share"])).unwrap();
        let body = serde_json::to_value(service.plan(request).await.unwrap()).unwrap();

        assert_eq!(timing.calls.load(Ordering::Relaxed), 1);
        assert_eq!(availability_calls.load(Ordering::Relaxed), 1);
        assert_eq!(street.direct_calls.load(Ordering::Relaxed), 1);
        let options = body["journey_options"].as_array().unwrap();
        assert_eq!(options.len(), 1);
        assert_eq!(leg_modes(&options[0]), vec!["walking"]);
        assert_eq!(options[0]["duration_seconds"], 3000);
    }
}

#[tokio::test]
async fn optional_internal_bike_availability_error_keeps_walking() {
    bike_only_walking_result(Err(BikeAvailabilityError::InvalidConfiguration)).await;
}

struct RankingStreet {
    inner: FakeStreetRouter,
    route_calls: AtomicUsize,
    matrix_calls: AtomicUsize,
    hydration_calls: AtomicUsize,
    origin: via_server::street::Coordinate,
    destination: via_server::street::Coordinate,
}

#[async_trait]
impl via_server::street::StreetRouter for RankingStreet {
    async fn matrix(
        &self,
        query: via_server::street::MatrixQuery,
    ) -> Result<via_server::street::MatrixResult, via_server::street::StreetRoutingError> {
        self.matrix_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.matrix(query).await
    }
    async fn route(
        &self,
        query: via_server::street::RouteQuery,
    ) -> Result<via_server::street::StreetPath, via_server::street::StreetRoutingError> {
        self.route_calls.fetch_add(1, Ordering::Relaxed);
        if (query.origin == self.origin || query.destination == self.destination)
            && !(query.origin == self.origin && query.destination == self.destination)
        {
            self.hydration_calls.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.route(query).await
    }
}

async fn ranking_corridor_fixture(rejected_by_ranking: bool, iterations: usize) {
    use time::{Duration, macros::datetime};
    use via_server::domain::{
        ids::{BoardingPointId, EffectiveTripId, TransitPatternId},
        realtime::{ArrivalEstimate, ArrivalState, FreshnessState, NormalizedRealtimeValue},
        transit_timing::{EffectiveStopTime, TimingBasis, TimingReliability},
    };
    use via_server::routing::{JourneyPlanningLimits, RoutingService};
    let mut transit = network();
    let red = transit
        .patterns
        .iter()
        .find(|p| p.id.as_str() == "fixture-mrt-red")
        .unwrap()
        .clone();
    let mut alternative = red.clone();
    alternative.id = TransitPatternId::new("alternative");
    alternative.stops = vec![
        BoardingPointId::new("F-M2-BLUE"),
        BoardingPointId::new("F-M3-BLUE"),
    ];
    let a = stop_coordinate(&transit, "F-M1");
    let b = stop_coordinate(&transit, "F-M2-RED");
    for (id, coordinate) in [("F-M2-BLUE", a), ("F-M3-BLUE", b)] {
        let stop = transit
            .stops
            .iter_mut()
            .find(|stop| stop.id.as_str() == id)
            .unwrap();
        stop.coordinate.latitude = coordinate.latitude() + 0.0002;
        stop.coordinate.longitude = coordinate.longitude() + 0.0002;
    }
    transit.patterns = vec![red.clone(), alternative.clone()];
    transit.trips.retain(|trip| trip.pattern_id == red.id);
    transit.transfer_edges.clear();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let mut street = FakeStreetRouter::builder().path(
        StreetTravelMode::Walking,
        origin,
        destination,
        path(origin, destination, 960),
    );
    let mut snapshot = TimingSnapshot::default();
    let departure = datetime!(2026-08-31 10:00 +08:00);
    for (index, pattern) in [red, alternative].into_iter().enumerate() {
        let access = stop_coordinate(&transit, pattern.stops[0].as_str());
        let egress = stop_coordinate(&transit, pattern.stops[1].as_str());
        let walk = if index == 0 { 60 } else { 30 };
        street = street
            .path(
                StreetTravelMode::Walking,
                origin,
                access,
                path(origin, access, walk),
            )
            .path(
                StreetTravelMode::Walking,
                egress,
                destination,
                path(egress, destination, walk),
            );
        let arrival = departure + Duration::seconds(if index == 0 { 720 } else { 760 });
        let evidence = if rejected_by_ranking {
            vec![NormalizedRealtimeValue {
                freshness: FreshnessState::Degraded,
                value: ArrivalEstimate {
                    transit_pattern_id: pattern.id.clone(),
                    boarding_point_id: pattern.stops[0].clone(),
                    predicted_arrival_at: Some(departure + Duration::minutes(2)),
                    state: ArrivalState::Predicted,
                    transmitted_at: departure,
                },
            }]
        } else {
            Vec::new()
        };
        snapshot.insert(
            pattern.id.clone(),
            0,
            vec![EffectiveTrip {
                id: EffectiveTripId::new(format!("rank-{index}")),
                pattern_id: pattern.id,
                scheduled_trip_id: None,
                stop_times: vec![
                    EffectiveStopTime {
                        stop_id: pattern.stops[0].clone(),
                        arrival_at: departure + Duration::minutes(2),
                        departure_at: departure + Duration::minutes(2),
                    },
                    EffectiveStopTime {
                        stop_id: pattern.stops[1].clone(),
                        arrival_at: arrival,
                        departure_at: arrival,
                    },
                ],
                timing_basis: TimingBasis::FrequencyEstimate,
                timing_reliability: TimingReliability::Fallback,
                observed_at: None,
                uncertainty_seconds: None,
                evidence,
            }],
        );
    }
    let street = Arc::new(RankingStreet {
        inner: street.build(),
        route_calls: AtomicUsize::new(0),
        matrix_calls: AtomicUsize::new(0),
        hydration_calls: AtomicUsize::new(0),
        origin,
        destination,
    });
    let timing = Arc::new(CountingTiming {
        calls: AtomicUsize::new(0),
        snapshot,
    });
    let mut limits = JourneyPlanningLimits::default();
    limits.planner.max_candidates = 1;
    let service = RoutingService::new(
        street.clone(),
        Arc::new(TransitRuntime::new(Arc::new(transit))),
        timing.clone(),
        Arc::new(BikeShareNetwork::new(Vec::new())),
        Arc::new(UnavailableBikeAvailabilitySource),
        limits,
        false,
    );
    let mut stable_key = None;
    let mut latency_us = Vec::new();
    for _ in 0..iterations {
        let started = std::time::Instant::now();
        let body = serde_json::to_value(
            service
                .plan(serde_json::from_value(request(origin, destination, &["metro"])).unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        latency_us.push(started.elapsed().as_micros());
        let options = body["journey_options"].as_array().unwrap();
        assert_eq!(options.len(), 1);
        assert_eq!(leg_modes(&options[0]), vec!["walking"]);
        assert_eq!(options[0]["duration_seconds"], 960);
        let key = options[0]["matching_key"].clone();
        if let Some(previous) = stable_key {
            assert_eq!(previous, key);
        }
        stable_key = Some(key);
    }
    assert_eq!(
        street.hydration_calls.load(Ordering::Relaxed),
        4 * iterations
    );
    assert_eq!(street.matrix_calls.load(Ordering::Relaxed), 2 * iterations);
    assert_eq!(timing.calls.load(Ordering::Relaxed), iterations);
    assert_eq!(street.route_calls.load(Ordering::Relaxed), 7 * iterations);
    latency_us.sort_unstable();
    eprintln!(
        "ranking fixture: reject={rejected_by_ranking} iterations={iterations} median_us={} p95_us={} routes_per_request={} matrices_per_request={} hydration_routes_per_request={} reconstructed_per_request=3",
        latency_us[iterations / 2],
        latency_us[iterations * 95 / 100],
        street.route_calls.load(Ordering::Relaxed) / iterations,
        street.matrix_calls.load(Ordering::Relaxed) / iterations,
        street.hydration_calls.load(Ordering::Relaxed) / iterations
    );
}

#[tokio::test]
async fn ranking_considers_three_corridors_before_applying_a_one_result_limit() {
    for rejected_by_ranking in [false, true] {
        ranking_corridor_fixture(rejected_by_ranking, 2).await;
    }
}

#[tokio::test]
#[ignore = "in-process reconstruction workload comparison"]
async fn ranking_reconstruction_benchmark() {
    for rejected_by_ranking in [false, true] {
        ranking_corridor_fixture(rejected_by_ranking, 1000).await;
    }
}
