use super::candidates::station_choices;

use time::macros::datetime;

use crate::{
    domain::location::GeoCoordinate,
    domain::{
        bikeshare::{
            BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
            BikeStationAvailability,
        },
        ids::{BikeStationId, BoardingPointId},
    },
    routing::cache::MatrixCache,
    street::FakeStreetRouter,
};

use super::*;

#[test]
fn configured_endpoint_radius_controls_snapshot_and_candidate_selection() {
    let now = datetime!(2026-09-21 10:00 UTC);
    let network = Arc::new(BikeShareNetwork::new(vec![
        BikeStation::new(
            BikeStationId::new("first"),
            "First",
            GeoCoordinate {
                latitude: 25.002,
                longitude: 121.5,
            },
        ),
        BikeStation::new(
            BikeStationId::new("second"),
            "Second",
            GeoCoordinate {
                latitude: 25.003,
                longitude: 121.5,
            },
        ),
    ]));
    let snapshot = BikeAvailabilitySnapshot::new(
        now,
        network
            .stations
            .iter()
            .map(|station| {
                (
                    station.id.clone(),
                    BikeStationAvailability::new(station.id.clone(), 1, 1, true, now),
                )
            })
            .collect(),
    );
    let from = CorridorAnchor {
        kind: crate::routing::journey::JourneyAnchorKind::Origin,
        coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
    };
    let to = CorridorAnchor {
        kind: crate::routing::journey::JourneyAnchorKind::Destination,
        coordinate: Coordinate::try_new(25.005, 121.5).unwrap(),
    };
    for (radius, expected) in [(100, 0), (300, 2)] {
        let config = crate::config::ViaConfig::from_toml(
            &include_str!("../../../../config/via.toml").replace(
                "endpoint_radius_metres = 1500",
                &format!("endpoint_radius_metres = {radius}"),
            ),
        )
        .unwrap();
        let planner = BikeShareMovementPlanner::new(network.clone(), config.bike_policy());
        assert_eq!(planner.required_station_ids([&from, &to]).len(), expected);
        assert_eq!(
            planner
                .movement_candidate(&from, &to, &snapshot, now)
                .is_some(),
            expected > 0
        );
    }
}

#[test]
fn candidate_discovery_rejects_missing_or_same_station_pairs() {
    let now = datetime!(2026-09-21 10:00 UTC);
    let station_id = BikeStationId::new("only-station");
    let network = Arc::new(BikeShareNetwork::new(vec![BikeStation::new(
        station_id.clone(),
        "Only station",
        GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    )]));
    let snapshot = BikeAvailabilitySnapshot::new(
        now,
        HashMap::from([(
            station_id.clone(),
            BikeStationAvailability::new(station_id, 1, 1, true, now),
        )]),
    );
    let planner =
        BikeShareMovementPlanner::new(network, crate::config::ViaConfig::default().bike_policy());
    let from = CorridorAnchor {
        kind: crate::routing::journey::JourneyAnchorKind::Origin,
        coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
    };
    let near = CorridorAnchor {
        kind: crate::routing::journey::JourneyAnchorKind::Destination,
        coordinate: Coordinate::try_new(25.001, 121.5).unwrap(),
    };
    let far = CorridorAnchor {
        coordinate: Coordinate::try_new(26.0, 121.5).unwrap(),
        ..near.clone()
    };
    assert!(
        planner
            .movement_candidate(&from, &near, &snapshot, now)
            .is_none()
    );
    assert!(
        planner
            .movement_candidate(&from, &far, &snapshot, now)
            .is_none()
    );
    assert!(
        planner
            .movement_candidate(&far, &near, &snapshot, now)
            .is_none()
    );
}

#[test]
fn required_stations_cover_all_anchors_and_availability_precedes_limits() {
    use crate::routing::journey::JourneyAnchorKind;
    let origin = Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = Coordinate::try_new(25.1, 121.5).unwrap();
    let mut stations = (0..4)
        .map(|index| {
            BikeStation::new(
                BikeStationId::new(format!("near-origin-{index}")),
                "Origin station",
                GeoCoordinate {
                    latitude: 25.0 + 0.0001 * f64::from(index + 1),
                    longitude: 121.5,
                },
            )
        })
        .collect::<Vec<_>>();
    stations.push(BikeStation::new(
        BikeStationId::new("near-destination"),
        "Destination station",
        GeoCoordinate {
            latitude: 25.1,
            longitude: 121.5,
        },
    ));
    stations.push(BikeStation::new(
        BikeStationId::new("outside-radius"),
        "Far away",
        GeoCoordinate {
            latitude: 24.0,
            longitude: 121.5,
        },
    ));
    let mut anchors = vec![CorridorAnchor {
        kind: JourneyAnchorKind::Origin,
        coordinate: origin,
    }];
    let mut links = Vec::new();
    for (index, name) in [
        "access",
        "intermediate",
        "transfer-from",
        "transfer-to",
        "egress",
    ]
    .into_iter()
    .enumerate()
    {
        let stop_id = BoardingPointId::new(name);
        let station_id = BikeStationId::new(format!("linked-{name}"));
        stations.push(BikeStation::new(
            station_id.clone(),
            name,
            GeoCoordinate {
                latitude: 24.0,
                longitude: 121.5,
            },
        ));
        links.push(BikeStationAccess::new(
            stop_id.clone(),
            station_id,
            (index % 2 == 0).then_some(BikeWalkingAccess::new(10, 20)),
            (index % 2 == 1).then_some(BikeWalkingAccess::new(10, 20)),
        ));
        anchors.push(CorridorAnchor {
            kind: JourneyAnchorKind::BoardingPoint { id: stop_id },
            coordinate: origin,
        });
    }
    anchors.push(CorridorAnchor {
        kind: JourneyAnchorKind::Destination,
        coordinate: destination,
    });
    for index in 0..4 {
        links.push(BikeStationAccess::new(
            BoardingPointId::new("access"),
            BikeStationId::new(format!("near-origin-{index}")),
            Some(BikeWalkingAccess::new(20 + index, 40 + index)),
            None,
        ));
    }
    let network = Arc::new(BikeShareNetwork::new(stations).with_access_links(links));
    let policy = BikeMovementPolicy {
        max_stations_per_endpoint: 3,
        ..crate::config::ViaConfig::default().bike_policy()
    };
    let planner = BikeShareMovementPlanner::new(network, policy.clone());
    let required = planner.required_station_ids(anchors.iter().chain(anchors.iter()));
    assert_eq!(required.len(), 10);
    assert!(!required.contains(&BikeStationId::new("outside-radius")));
    for name in [
        "access",
        "intermediate",
        "transfer-from",
        "transfer-to",
        "egress",
    ] {
        assert!(required.contains(&BikeStationId::new(format!("linked-{name}"))));
    }
    let now = datetime!(2026-09-21 10:00 UTC);
    let mut snapshot = BikeAvailabilitySnapshot::new(
        now,
        required
            .into_iter()
            .map(|id| {
                let bikes = u32::from(id.as_str() == "near-origin-3");
                (
                    id.clone(),
                    BikeStationAvailability::new(id, bikes, 1, true, now),
                )
            })
            .collect(),
    );
    let candidate = planner
        .movement_candidate(&anchors[0], anchors.last().unwrap(), &snapshot, now)
        .unwrap();
    assert_eq!(candidate.pickups.len(), 1);
    assert_eq!(candidate.pickups[0].station_id.as_str(), "near-origin-3");
    let boarding_candidate = planner
        .movement_candidate(&anchors[1], anchors.last().unwrap(), &snapshot, now)
        .unwrap();
    assert_eq!(boarding_candidate.pickups.len(), 1);
    assert_eq!(
        boarding_candidate.pickups[0].station_id.as_str(),
        "near-origin-3"
    );
    for availability in snapshot.stations.values_mut() {
        availability.available_bikes = 1;
    }
    let candidate = planner
        .movement_candidate(&anchors[0], anchors.last().unwrap(), &snapshot, now)
        .unwrap();
    assert_eq!(candidate.pickups.len(), policy.max_stations_per_endpoint);
    assert_eq!(candidate.pickups[0].station_id.as_str(), "near-origin-0");
    let boarding_candidate = planner
        .movement_candidate(&anchors[1], anchors.last().unwrap(), &snapshot, now)
        .unwrap();
    assert_eq!(
        boarding_candidate.pickups.len(),
        policy.max_pickup_stations_per_anchor
    );
}

#[test]
fn station_choices_are_bounded_to_three_nearest_per_anchor() {
    let anchor = BoardingPointId::new("anchor");
    let stations = (0..5)
        .map(|index| {
            BikeStation::new(
                BikeStationId::new(format!("station-{index}")),
                format!("Station {index}"),
                GeoCoordinate {
                    latitude: 25.0,
                    longitude: 121.5,
                },
            )
        })
        .collect::<Vec<_>>();
    let access_links = (0..5)
        .map(|index| {
            BikeStationAccess::new(
                anchor.clone(),
                BikeStationId::new(format!("station-{index}")),
                Some(BikeWalkingAccess::new(index + 1, index + 1)),
                Some(BikeWalkingAccess::new(index + 1, index + 1)),
            )
        })
        .collect();
    let network = BikeShareNetwork::new(stations).with_access_links(access_links);
    let pickups = station_choices(
        network.access_from_boarding_point(&anchor),
        true,
        &BikeMovementPolicy::default(),
    );
    let dropoffs = station_choices(
        network.access_to_boarding_point(&anchor),
        false,
        &BikeMovementPolicy::default(),
    );

    assert_eq!(
        pickups.len(),
        crate::config::ViaConfig::default()
            .bikeshare
            .max_pickup_stations_per_anchor
    );
    assert_eq!(
        dropoffs.len(),
        crate::config::ViaConfig::default()
            .bikeshare
            .max_dropoff_stations_per_anchor
    );
    assert_eq!(
        pickups
            .iter()
            .map(|choice| choice.station_id.as_str())
            .collect::<Vec<_>>(),
        vec!["station-0", "station-1", "station-2"]
    );
}

#[tokio::test]
async fn cached_bike_availability_is_checked_at_the_request_time() {
    let from = BoardingPointId::new("from");
    let to = BoardingPointId::new("to");
    let pickup_id = BikeStationId::new("tdx:Taipei:pickup");
    let dropoff_id = BikeStationId::new("tdx:Taipei:dropoff");
    let bike_network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(
                pickup_id.clone(),
                "Pickup",
                GeoCoordinate {
                    latitude: 25.0,
                    longitude: 121.5,
                },
            ),
            BikeStation::new(
                dropoff_id.clone(),
                "Dropoff",
                GeoCoordinate {
                    latitude: 25.001,
                    longitude: 121.501,
                },
            ),
        ])
        .with_access_links(vec![
            BikeStationAccess::new(
                from.clone(),
                pickup_id.clone(),
                Some(BikeWalkingAccess::new(10, 50)),
                None,
            ),
            BikeStationAccess::new(
                to.clone(),
                dropoff_id.clone(),
                None,
                Some(BikeWalkingAccess::new(10, 50)),
            ),
        ])
        .with_revision("fixture"),
    );
    let received_at = datetime!(2026-09-21 10:00 UTC);
    let mut stations = HashMap::new();
    for (station_id, bikes, docks) in [(pickup_id, 1, 0), (dropoff_id, 0, 1)] {
        stations.insert(
            station_id.clone(),
            BikeStationAvailability::new(
                station_id,
                bikes,
                docks,
                true,
                received_at - time::Duration::seconds(110),
            ),
        );
    }
    let snapshot = BikeAvailabilitySnapshot::new(received_at, stations);
    let planner = BikeShareMovementPlanner::new(
        bike_network,
        BikeMovementPolicy {
            availability_max_age_seconds: 120,
            ..crate::config::ViaConfig::default().bike_policy()
        },
    );

    let from_endpoint = CorridorAnchor {
        kind: crate::routing::journey::JourneyAnchorKind::BoardingPoint { id: from.clone() },
        coordinate: Coordinate::try_new(25.0, 121.5).unwrap(),
    };
    let to_endpoint = CorridorAnchor {
        kind: crate::routing::journey::JourneyAnchorKind::BoardingPoint { id: to.clone() },
        coordinate: Coordinate::try_new(25.001, 121.501).unwrap(),
    };
    let candidate = planner
        .movement_candidate(&from_endpoint, &to_endpoint, &snapshot, received_at)
        .unwrap();
    assert_eq!(candidate.pickups.len(), 1);
    assert_eq!(candidate.dropoffs.len(), 1);
    assert_ne!(
        candidate.pickups[0].station_id,
        candidate.dropoffs[0].station_id
    );
    let street = MatrixCache::new(Arc::new(FakeStreetRouter::builder().build()));
    planner.realize(&street, &candidate).await.unwrap();
    assert_eq!(street.requests(), 1);
    assert!(
        planner
            .movement_candidate(
                &from_endpoint,
                &to_endpoint,
                &snapshot,
                received_at + time::Duration::seconds(11),
            )
            .is_none()
    );
}
#[tokio::test]
async fn generic_endpoints_share_bicycle_realization_and_cache() {
    use crate::routing::journey::JourneyAnchorKind;
    use crate::street::{Metres, Seconds};
    let now = datetime!(2026-09-21 10:00 UTC);
    let origin = Coordinate::try_new(25.0, 121.5).unwrap();
    let pickup = Coordinate::try_new(25.0001, 121.5).unwrap();
    let dropoff = Coordinate::try_new(25.01, 121.5).unwrap();
    let to_id = BoardingPointId::new("to");
    let pickup_id = BikeStationId::new("pickup");
    let dropoff_id = BikeStationId::new("dropoff");
    let network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(
                pickup_id.clone(),
                "Pickup",
                GeoCoordinate {
                    latitude: pickup.latitude(),
                    longitude: pickup.longitude(),
                },
            ),
            BikeStation::new(
                dropoff_id.clone(),
                "Dropoff",
                GeoCoordinate {
                    latitude: dropoff.latitude(),
                    longitude: dropoff.longitude(),
                },
            ),
        ])
        .with_access_links(vec![BikeStationAccess::new(
            to_id.clone(),
            dropoff_id.clone(),
            None,
            Some(BikeWalkingAccess::new(20, 30)),
        )]),
    );
    let snapshot = BikeAvailabilitySnapshot::new(
        now,
        HashMap::from([
            (
                pickup_id.clone(),
                BikeStationAvailability::new(pickup_id, 1, 0, true, now),
            ),
            (
                dropoff_id.clone(),
                BikeStationAvailability::new(dropoff_id, 0, 1, true, now),
            ),
        ]),
    );
    let street = Arc::new(MatrixCache::new(Arc::new(
        FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                origin,
                pickup,
                Seconds(10),
                Metres(12),
            )
            .matrix_leg(
                StreetTravelMode::Bicycle,
                pickup,
                dropoff,
                Seconds(100),
                Metres(1000),
            )
            .build(),
    )));
    let planner =
        BikeShareMovementPlanner::new(network, crate::config::ViaConfig::default().bike_policy());
    let from = CorridorAnchor {
        kind: JourneyAnchorKind::Origin,
        coordinate: origin,
    };
    let to = CorridorAnchor {
        kind: JourneyAnchorKind::BoardingPoint { id: to_id },
        coordinate: dropoff,
    };
    let session = BikeShareSession::new(
        planner.clone(),
        Arc::new(snapshot.clone()),
        now,
        street.clone(),
    );
    let movements = session.movements(&from, &to, 3, 4).await.unwrap();
    let bike_movement = &movements[0];
    assert_eq!(
        bike_movement.total_seconds,
        130 + crate::config::ViaConfig::default()
            .bikeshare
            .rental_handling_seconds
    );
    assert_eq!((bike_movement.from_anchor, bike_movement.to_anchor), (3, 4));
    assert_eq!(street.requests(), 2);
    let remapped = session.movements(&from, &to, 0, 1).await.unwrap();
    let bike_movement = &remapped[0];
    assert_eq!((bike_movement.from_anchor, bike_movement.to_anchor), (0, 1));
    assert_eq!((session.requests(), session.hits()), (1, 1));
    assert_eq!(street.requests(), 2);
    let unavailable = BikeShareSession::new(
        planner,
        Arc::new(BikeAvailabilitySnapshot::new(now, HashMap::new())),
        now,
        street.clone(),
    );
    assert!(
        unavailable
            .movements(&from, &to, 3, 4)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn station_walks_over_fifteen_minutes_are_unavailable() {
    use crate::street::{Metres, Seconds};
    assert!(
        super::realization::walking_access(
            &MatrixCell::Reachable {
                duration: Seconds(901),
                distance: Metres(1)
            },
            900
        )
        .is_none()
    );
    assert!(
        super::realization::walking_access(
            &MatrixCell::Reachable {
                duration: Seconds(900),
                distance: Metres(1)
            },
            900
        )
        .is_some()
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_candidate_discovery_caches_none_once() {
    let now = datetime!(2026-09-21 10:00 UTC);
    let session = Arc::new(BikeShareSession::new(
        BikeShareMovementPlanner::new(
            Arc::new(BikeShareNetwork::new(Vec::new())),
            crate::config::ViaConfig::default().bike_policy(),
        ),
        Arc::new(BikeAvailabilitySnapshot::new(now, HashMap::new())),
        now,
        Arc::new(FakeStreetRouter::builder().build()),
    ));
    let barrier = Arc::new(tokio::sync::Barrier::new(10));
    let tasks = (0..10)
        .map(|_| {
            let session = session.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let corridor = crate::routing::evaluation::EvaluationCorridor::direct_walking(
                    Coordinate::try_new(25.0, 121.5).unwrap(),
                    Coordinate::try_new(25.01, 121.5).unwrap(),
                );
                barrier.wait().await;
                let movements = session
                    .movements(&corridor.anchors()[0], &corridor.anchors()[1], 0, 1)
                    .await
                    .unwrap();
                assert!(movements.is_empty());
            })
        })
        .collect::<Vec<_>>();
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!((session.requests(), session.hits()), (1, 9));
}
