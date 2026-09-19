use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use serde_json::json;
use time::{Duration, OffsetDateTime, macros::datetime};
use via_server::{
    api::{DepartureSetting, PlanningRequest, RequestCoordinate, SelectedPlace, TransportMode},
    domain::{
        bikeshare::{
            BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
            BikeStationAvailability, BikeWalkingAccess,
        },
        ids::{BoardingPointId, EffectiveTripId, TransitServiceId},
        transit_timing::{EffectiveStopTime, EffectiveTrip, TimingBasis, TimingReliability},
    },
    providers::{BikeAvailabilityError, BikeAvailabilitySource},
    routing::{
        AccessPoint, BikeAugmentationPolicy, BikeJourneyAugmenter, EgressPoint, JourneyAnchorKind,
        JourneyCandidate, JourneyMovementKind, JourneyPathMovement, JourneySegment,
        JourneySkeleton, JourneyVariant, TransferShortcut,
    },
    street::{
        FakeStreetRouter, Maneuver, MatrixCell, MatrixOrientation, MatrixQuery, MatrixResult,
        Metres, RouteQuery, Seconds, StreetPath, StreetRouter, StreetRoutingError,
        StreetTravelMode,
    },
};

use super::fixtures::{self, FixtureRealtime};
use crate::support::routing::{near_stop, path, stop_coordinate};

const START: OffsetDateTime = datetime!(2026-08-31 10:00 UTC);

fn request() -> PlanningRequest {
    PlanningRequest {
        origin: SelectedPlace {
            coordinate: RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
            display_text: "Origin".into(),
        },
        destination: SelectedPlace {
            coordinate: RequestCoordinate {
                latitude: 25.01,
                longitude: 121.51,
            },
            display_text: "Destination".into(),
        },
        selected_modes: vec![TransportMode::Bus],
        locale: "en".into(),
        departure_setting: DepartureSetting::DepartAt { time: START },
    }
}

fn trip(id: &str, service_id: &str, stops: &[&str], offsets: &[(i64, i64)]) -> EffectiveTrip {
    EffectiveTrip {
        id: EffectiveTripId::new(id),
        service_id: TransitServiceId::new(service_id),
        scheduled_trip_id: None,
        stop_times: stops
            .iter()
            .zip(offsets)
            .map(|(stop_id, (arrival, departure))| EffectiveStopTime {
                stop_id: BoardingPointId::new(*stop_id),
                arrival_at: START + Duration::seconds(*arrival),
                departure_at: START + Duration::seconds(*departure),
            })
            .collect(),
        timing_basis: TimingBasis::Schedule,
        timing_reliability: TimingReliability::Reliable,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    }
}

fn candidate() -> JourneyCandidate {
    JourneyCandidate {
        access: AccessPoint::new(
            BoardingPointId::new("A"),
            START,
            START + Duration::seconds(30),
            30,
            150,
        ),
        egress: EgressPoint::new(BoardingPointId::new("D"), 45, 225),
        segments: vec![
            JourneySegment::Transit {
                trip: trip("trip-1", "service-1", &["A", "B"], &[(60, 60), (360, 360)]),
                board_index: 0,
                alight_index: 1,
            },
            JourneySegment::WalkingTransfer {
                shortcut: TransferShortcut::new(
                    BoardingPointId::new("B"),
                    BoardingPointId::new("C"),
                    120,
                ),
            },
            JourneySegment::Transit {
                trip: trip(
                    "trip-2",
                    "service-2",
                    &["C", "D"],
                    &[(600, 600), (960, 960)],
                ),
                board_index: 0,
                alight_index: 1,
            },
        ],
        departure_time: START,
        arrival_time: START + Duration::seconds(1_005),
        walking_seconds: 195,
        transfer_count: 1,
        uncertainty_seconds: None,
        evidence: Vec::new(),
        timing_bases: vec![TimingBasis::Schedule, TimingBasis::Schedule],
        timing_reliability: TimingReliability::Reliable,
    }
}

#[test]
fn direct_walking_projects_to_a_self_sufficient_skeleton() {
    let request = request();
    let origin = via_server::street::Coordinate::try_new(
        request.origin.coordinate.latitude,
        request.origin.coordinate.longitude,
    )
    .unwrap();
    let destination = via_server::street::Coordinate::try_new(
        request.destination.coordinate.latitude,
        request.destination.coordinate.longitude,
    )
    .unwrap();
    let path = StreetPath {
        duration: Seconds(300),
        distance: Metres(1_500),
        geometry: vec![origin, destination],
        maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
        attribution: Vec::new(),
    };

    let skeleton = JourneySkeleton::from_direct_walking(&request, path.clone(), START).unwrap();
    assert_eq!(skeleton.anchors().len(), 2);
    assert_eq!(skeleton.movements().len(), 1);
    assert!(matches!(
        skeleton.movements()[0].kind,
        JourneyMovementKind::Walking {
            kind: via_server::routing::WalkingMovementKind::Direct,
            duration_seconds: 300,
            distance_metres: 1_500,
        }
    ));
    assert_eq!(skeleton.movements()[0].street_path.as_deref(), Some(&path));
    assert_eq!(
        JourneyVariant::baseline(skeleton)
            .evaluate_timing()
            .unwrap()
            .arrival_at,
        START + Duration::seconds(300)
    );
}

#[derive(Clone)]
struct FixtureAvailabilitySource {
    snapshot: Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>,
}

#[derive(Clone)]
struct CountingMatrixRouter {
    matrix_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl StreetRouter for CountingMatrixRouter {
    async fn matrix(&self, _query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        self.matrix_calls.fetch_add(1, Ordering::Relaxed);
        Err(StreetRoutingError::Timeout)
    }

    async fn route(&self, _query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        Err(StreetRoutingError::Unreachable)
    }
}

#[derive(Clone, Copy)]
struct EndpointFailureRouter {
    fail_origin: bool,
    fail_destination: bool,
}

#[async_trait]
impl StreetRouter for EndpointFailureRouter {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        query.validate()?;
        if (self.fail_origin
            && query.travel_mode == StreetTravelMode::Walking
            && query.orientation == MatrixOrientation::OneToMany)
            || (self.fail_destination
                && query.travel_mode == StreetTravelMode::Walking
                && query.orientation == MatrixOrientation::ManyToOne)
        {
            return Err(StreetRoutingError::Timeout);
        }
        let cell = if query.travel_mode == StreetTravelMode::Bicycle {
            MatrixCell::Reachable {
                duration: Seconds(100),
                distance: Metres(600),
            }
        } else {
            MatrixCell::Reachable {
                duration: Seconds(20),
                distance: Metres(100),
            }
        };
        Ok(MatrixResult {
            cells: query
                .sources
                .iter()
                .map(|_| vec![cell; query.targets.len()])
                .collect(),
        })
    }

    async fn route(&self, _query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        Err(StreetRoutingError::Unreachable)
    }
}

#[async_trait]
impl BikeAvailabilitySource for FixtureAvailabilitySource {
    async fn snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        self.snapshot.clone()
    }
}

#[derive(Clone)]
struct CountingAvailabilitySource {
    snapshot: Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BikeAvailabilitySource for CountingAvailabilitySource {
    async fn snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.snapshot.clone()
    }
}

fn bike_network() -> (Arc<BikeShareNetwork>, RequestCoordinate, RequestCoordinate) {
    let pickup_coordinate = RequestCoordinate {
        latitude: 25.001,
        longitude: 121.501,
    };
    let dropoff_coordinate = RequestCoordinate {
        latitude: 25.002,
        longitude: 121.502,
    };
    let pickup_id = via_server::domain::ids::BikeStationId::new("pickup");
    let dropoff_id = via_server::domain::ids::BikeStationId::new("dropoff");
    let network = BikeShareNetwork::new(vec![
        BikeStation::new(pickup_id.clone(), "Pickup", pickup_coordinate),
        BikeStation::new(dropoff_id.clone(), "Dropoff", dropoff_coordinate),
    ])
    .with_access_links(vec![
        BikeStationAccess::new(
            BoardingPointId::new("A"),
            pickup_id,
            Some(BikeWalkingAccess::new(10, 50)),
            None,
        ),
        BikeStationAccess::new(
            BoardingPointId::new("C"),
            dropoff_id,
            None,
            Some(BikeWalkingAccess::new(10, 50)),
        ),
    ])
    .with_revision("fixture");
    (Arc::new(network), pickup_coordinate, dropoff_coordinate)
}

fn bike_snapshot(now: OffsetDateTime, stale: bool) -> Arc<BikeAvailabilitySnapshot> {
    let observed_at = if stale {
        now - Duration::seconds(121)
    } else {
        now
    };
    let mut stations = HashMap::new();
    stations.insert(
        via_server::domain::ids::BikeStationId::new("pickup"),
        BikeStationAvailability::new(
            via_server::domain::ids::BikeStationId::new("pickup"),
            2,
            0,
            true,
            observed_at,
        ),
    );
    stations.insert(
        via_server::domain::ids::BikeStationId::new("dropoff"),
        BikeStationAvailability::new(
            via_server::domain::ids::BikeStationId::new("dropoff"),
            0,
            2,
            true,
            observed_at,
        ),
    );
    Arc::new(BikeAvailabilitySnapshot::new(now, stations))
}

fn endpoint_bike_network(
    origin: RequestCoordinate,
    destination: RequestCoordinate,
) -> (Arc<BikeShareNetwork>, RequestCoordinate, RequestCoordinate) {
    let pickup_coordinate = RequestCoordinate {
        latitude: origin.latitude + 0.0001,
        longitude: origin.longitude + 0.0001,
    };
    let dropoff_coordinate = RequestCoordinate {
        latitude: destination.latitude + 0.0001,
        longitude: destination.longitude + 0.0001,
    };
    let pickup_id = via_server::domain::ids::BikeStationId::new("pickup");
    let dropoff_id = via_server::domain::ids::BikeStationId::new("dropoff");
    let network = BikeShareNetwork::new(vec![
        BikeStation::new(pickup_id, "Endpoint Pickup", pickup_coordinate),
        BikeStation::new(dropoff_id, "Endpoint Dropoff", dropoff_coordinate),
    ])
    .with_revision("fixture-endpoints");
    (Arc::new(network), pickup_coordinate, dropoff_coordinate)
}

fn endpoint_and_boarding_bike_network(
    origin: RequestCoordinate,
    destination: RequestCoordinate,
) -> Arc<BikeShareNetwork> {
    let pickup_id = via_server::domain::ids::BikeStationId::new("pickup");
    let dropoff_id = via_server::domain::ids::BikeStationId::new("dropoff");
    Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(
                pickup_id.clone(),
                "Endpoint and boarding pickup",
                RequestCoordinate {
                    latitude: origin.latitude + 0.0001,
                    longitude: origin.longitude + 0.0001,
                },
            ),
            BikeStation::new(
                dropoff_id.clone(),
                "Endpoint and boarding dropoff",
                RequestCoordinate {
                    latitude: destination.latitude + 0.0001,
                    longitude: destination.longitude + 0.0001,
                },
            ),
        ])
        .with_access_links(vec![
            BikeStationAccess::new(
                BoardingPointId::new("A"),
                pickup_id,
                Some(BikeWalkingAccess::new(10, 50)),
                None,
            ),
            BikeStationAccess::new(
                BoardingPointId::new("C"),
                dropoff_id,
                None,
                Some(BikeWalkingAccess::new(10, 50)),
            ),
        ])
        .with_revision("fixture-endpoints-and-boarding"),
    )
}

#[test]
fn candidate_projects_to_ordered_anchors_and_baseline_movements() {
    let skeleton = JourneySkeleton::from_candidate(&request(), candidate()).unwrap();

    assert_eq!(skeleton.anchors().len(), 6);
    assert!(matches!(
        skeleton.anchors()[0].kind,
        JourneyAnchorKind::Origin { .. }
    ));
    assert!(
        matches!(skeleton.anchors()[1].kind, JourneyAnchorKind::BoardingPoint { ref id } if id.as_str() == "A")
    );
    assert!(
        matches!(skeleton.anchors()[2].kind, JourneyAnchorKind::BoardingPoint { ref id } if id.as_str() == "B")
    );
    assert!(
        matches!(skeleton.anchors()[3].kind, JourneyAnchorKind::BoardingPoint { ref id } if id.as_str() == "C")
    );
    assert!(
        matches!(skeleton.anchors()[4].kind, JourneyAnchorKind::BoardingPoint { ref id } if id.as_str() == "D")
    );
    assert!(matches!(
        skeleton.anchors()[5].kind,
        JourneyAnchorKind::Destination { .. }
    ));
    assert_eq!(
        skeleton
            .anchors()
            .iter()
            .map(|anchor| anchor.reached_at)
            .collect::<Vec<_>>(),
        vec![
            START,
            START + Duration::seconds(30),
            START + Duration::seconds(360),
            START + Duration::seconds(480),
            START + Duration::seconds(960),
            START + Duration::seconds(1_005),
        ]
    );

    let movements = skeleton.movements();
    assert_eq!(movements.len(), 5);
    assert!(matches!(
        movements[0].kind,
        JourneyMovementKind::Walking {
            kind: via_server::routing::WalkingMovementKind::Access,
            duration_seconds: 30,
            distance_metres: 150
        }
    ));
    assert!(matches!(
        movements[1].kind,
        JourneyMovementKind::Transit {
            board_index: 0,
            alight_index: 1,
            ..
        }
    ));
    assert!(matches!(
        movements[2].kind,
        JourneyMovementKind::Walking {
            kind: via_server::routing::WalkingMovementKind::Transfer,
            duration_seconds: 120,
            ..
        }
    ));
    assert!(matches!(
        movements[3].kind,
        JourneyMovementKind::Transit {
            board_index: 0,
            alight_index: 1,
            ..
        }
    ));
    assert!(matches!(
        movements[4].kind,
        JourneyMovementKind::Walking {
            kind: via_server::routing::WalkingMovementKind::Egress,
            duration_seconds: 45,
            distance_metres: 225
        }
    ));
    assert_eq!(
        movements
            .iter()
            .map(|movement| (movement.from_anchor, movement.to_anchor))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)]
    );
}

#[tokio::test]
async fn bike_augmenter_builds_a_boarding_point_shortcut_and_keeps_baseline() {
    let skeleton = JourneySkeleton::from_candidate(&request(), candidate()).unwrap();
    let (network, pickup_coordinate, dropoff_coordinate) = bike_network();
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Bicycle,
            via_server::street::Coordinate::try_new(
                pickup_coordinate.latitude,
                pickup_coordinate.longitude,
            )
            .unwrap(),
            via_server::street::Coordinate::try_new(
                dropoff_coordinate.latitude,
                dropoff_coordinate.longitude,
            )
            .unwrap(),
            Seconds(100),
            Metres(600),
        )
        .build();
    let source = FixtureAvailabilitySource {
        snapshot: Ok(bike_snapshot(START, false)),
    };
    let augmenter = BikeJourneyAugmenter::new(
        network,
        Arc::new(source),
        Arc::new(street),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    );

    let variants = augmenter.augment(skeleton).await;

    assert!(variants.iter().any(|variant| variant.is_baseline()));
    let bike = variants
        .iter()
        .find(|variant| !variant.is_baseline())
        .expect("bike variant");
    assert_eq!(bike.path().len(), 4);
    assert!(matches!(
        bike.path()[1].movement,
        JourneyPathMovement::BikeShare { ref shortcut }
            if shortcut.from_anchor == 1
                && shortcut.to_anchor == 3
                && shortcut.total_seconds == 120
    ));
    let timing = bike.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, START + Duration::seconds(1_005));
    assert_eq!(timing.bike_share_uses, 1);
    assert_eq!(timing.bicycle_seconds, 100);
}

#[tokio::test]
async fn bike_augmenter_rejects_stale_availability_and_matrix_failures() {
    let (network, pickup_coordinate, dropoff_coordinate) = bike_network();
    let street = FakeStreetRouter::builder()
        .matrix_failure(via_server::street::StreetRoutingError::Timeout)
        .matrix_leg(
            StreetTravelMode::Bicycle,
            via_server::street::Coordinate::try_new(
                pickup_coordinate.latitude,
                pickup_coordinate.longitude,
            )
            .unwrap(),
            via_server::street::Coordinate::try_new(
                dropoff_coordinate.latitude,
                dropoff_coordinate.longitude,
            )
            .unwrap(),
            Seconds(100),
            Metres(600),
        )
        .build();
    let policy = BikeAugmentationPolicy {
        now: Some(START),
        ..BikeAugmentationPolicy::default()
    };

    let stale = BikeJourneyAugmenter::new(
        network.clone(),
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, true)),
        }),
        Arc::new(street.clone()),
        policy.clone(),
    );
    let stale_variants = stale
        .augment(JourneySkeleton::from_candidate(&request(), candidate()).unwrap())
        .await;
    assert_eq!(stale_variants.len(), 1);
    assert!(stale_variants[0].is_baseline());

    let failed = BikeJourneyAugmenter::new(
        network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        Arc::new(street),
        policy,
    );
    let failed_variants = failed
        .augment(JourneySkeleton::from_candidate(&request(), candidate()).unwrap())
        .await;
    assert_eq!(failed_variants.len(), 1);
    assert!(failed_variants[0].is_baseline());
}

#[tokio::test]
async fn bike_augmenter_builds_a_pure_endpoint_to_endpoint_variant() {
    let origin = request().origin.coordinate;
    let destination = request().destination.coordinate;
    let (network, pickup_coordinate, dropoff_coordinate) =
        endpoint_bike_network(origin, destination);
    let origin_coordinate =
        via_server::street::Coordinate::try_new(origin.latitude, origin.longitude).unwrap();
    let destination_coordinate =
        via_server::street::Coordinate::try_new(destination.latitude, destination.longitude)
            .unwrap();
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            origin_coordinate,
            pickup,
            Seconds(20),
            Metres(100),
        )
        .matrix_leg(
            StreetTravelMode::Walking,
            dropoff,
            destination_coordinate,
            Seconds(25),
            Metres(125),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(90),
            Metres(600),
        )
        .build();
    let augmenter = BikeJourneyAugmenter::new(
        network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        Arc::new(street),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    );

    let variants = augmenter
        .augment(JourneySkeleton::from_candidate(&request(), candidate()).unwrap())
        .await;

    let bike = variants
        .iter()
        .find(|variant| !variant.is_baseline())
        .expect("pure endpoint bike variant");
    assert_eq!(bike.path().len(), 1);
    assert!(matches!(
        bike.path()[0].movement,
        JourneyPathMovement::BikeShare { ref shortcut }
            if shortcut.from_anchor == 0
                && shortcut.to_anchor == 5
                && shortcut.total_seconds == 135
    ));
    let timing = bike.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, START + Duration::seconds(135));
    assert_eq!(timing.bike_share_uses, 1);
    assert_eq!(timing.bicycle_seconds, 90);
}

#[tokio::test]
async fn origin_bike_shortcut_can_replace_access_and_transit_until_a_later_stop() {
    let origin = via_server::street::Coordinate::try_new(25.0, 121.5).unwrap();
    let pickup_coordinate = RequestCoordinate {
        latitude: origin.latitude() + 0.0001,
        longitude: origin.longitude() + 0.0001,
    };
    let c = via_server::street::Coordinate::try_new(25.004, 121.504).unwrap();
    let dropoff_coordinate = RequestCoordinate {
        latitude: c.latitude() + 0.0001,
        longitude: c.longitude() + 0.0001,
    };
    let pickup_id = via_server::domain::ids::BikeStationId::new("pickup");
    let dropoff_id = via_server::domain::ids::BikeStationId::new("dropoff");
    let network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(pickup_id.clone(), "Pickup", pickup_coordinate),
            BikeStation::new(dropoff_id.clone(), "Dropoff", dropoff_coordinate),
        ])
        .with_access_links(vec![BikeStationAccess::new(
            BoardingPointId::new("C"),
            dropoff_id,
            None,
            Some(BikeWalkingAccess::new(10, 50)),
        )])
        .with_revision("fixture-origin-later"),
    );
    let origin_coordinate =
        via_server::street::Coordinate::try_new(origin.latitude(), origin.longitude()).unwrap();
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            origin_coordinate,
            pickup,
            Seconds(5),
            Metres(25),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(60),
            Metres(400),
        )
        .build();
    let augmenter = BikeJourneyAugmenter::new(
        network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        Arc::new(street),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    );

    let variants = augmenter
        .augment(JourneySkeleton::from_candidate(&request(), candidate()).unwrap())
        .await;
    let bike = variants
        .iter()
        .find(|variant| !variant.is_baseline())
        .expect("origin-to-later-stop bike variant");
    assert!(matches!(
        bike.path()[0].movement,
        JourneyPathMovement::BikeShare { ref shortcut }
            if shortcut.from_anchor == 0 && shortcut.to_anchor == 3
    ));
    assert!(matches!(
        bike.path()[1].movement,
        JourneyPathMovement::Baseline { movement_index: 3 }
    ));
    assert!(bike.evaluate_timing().is_ok());
}

#[tokio::test]
async fn boarding_point_bike_shortcut_can_replace_transit_and_egress() {
    let destination = request().destination.coordinate;
    let b = RequestCoordinate {
        latitude: 25.0,
        longitude: 121.5,
    };
    let pickup_coordinate = RequestCoordinate {
        latitude: b.latitude + 0.0001,
        longitude: b.longitude + 0.0001,
    };
    let dropoff_coordinate = RequestCoordinate {
        latitude: destination.latitude + 0.0001,
        longitude: destination.longitude + 0.0001,
    };
    let pickup_id = via_server::domain::ids::BikeStationId::new("pickup");
    let dropoff_id = via_server::domain::ids::BikeStationId::new("dropoff");
    let network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(pickup_id.clone(), "Pickup", pickup_coordinate),
            BikeStation::new(dropoff_id.clone(), "Dropoff", dropoff_coordinate),
        ])
        .with_access_links(vec![BikeStationAccess::new(
            BoardingPointId::new("B"),
            pickup_id,
            Some(BikeWalkingAccess::new(10, 50)),
            None,
        )])
        .with_revision("fixture-board-to-destination"),
    );
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let destination_coordinate =
        via_server::street::Coordinate::try_new(destination.latitude, destination.longitude)
            .unwrap();
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            dropoff,
            destination_coordinate,
            Seconds(5),
            Metres(25),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(60),
            Metres(400),
        )
        .build();
    let augmenter = BikeJourneyAugmenter::new(
        network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        Arc::new(street),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    );

    let variants = augmenter
        .augment(JourneySkeleton::from_candidate(&request(), candidate()).unwrap())
        .await;
    let bike = variants
        .iter()
        .find(|variant| !variant.is_baseline())
        .expect("boarding-point-to-destination bike variant");
    assert!(matches!(
        bike.path()[0].movement,
        JourneyPathMovement::Baseline { movement_index: 0 }
    ));
    assert!(matches!(
        bike.path()[1].movement,
        JourneyPathMovement::Baseline { movement_index: 1 }
    ));
    assert!(matches!(
        bike.path()[2].movement,
        JourneyPathMovement::BikeShare { ref shortcut }
            if shortcut.from_anchor == 2 && shortcut.to_anchor == 5
    ));
    assert!(bike.evaluate_timing().is_ok());
}

#[tokio::test]
async fn endpoint_bike_matrix_directionality_is_preserved() {
    let origin = request().origin.coordinate;
    let destination = request().destination.coordinate;
    let (network, pickup_coordinate, dropoff_coordinate) =
        endpoint_bike_network(origin, destination);
    let origin_coordinate =
        via_server::street::Coordinate::try_new(origin.latitude, origin.longitude).unwrap();
    let destination_coordinate =
        via_server::street::Coordinate::try_new(destination.latitude, destination.longitude)
            .unwrap();
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            pickup,
            origin_coordinate,
            Seconds(20),
            Metres(100),
        )
        .matrix_leg(
            StreetTravelMode::Walking,
            dropoff,
            destination_coordinate,
            Seconds(25),
            Metres(125),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(90),
            Metres(600),
        )
        .build();
    let augmenter = BikeJourneyAugmenter::new(
        network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        Arc::new(street),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    );

    let variants = augmenter
        .augment(JourneySkeleton::from_candidate(&request(), candidate()).unwrap())
        .await;

    assert!(variants.iter().all(|variant| variant.is_baseline()));
}

#[tokio::test]
async fn unavailable_endpoint_stations_skip_endpoint_matrix_work() {
    let origin = request().origin.coordinate;
    let destination = request().destination.coordinate;
    let (network, _, _) = endpoint_bike_network(origin, destination);
    let mut stations = HashMap::new();
    stations.insert(
        via_server::domain::ids::BikeStationId::new("pickup"),
        BikeStationAvailability::new(
            via_server::domain::ids::BikeStationId::new("pickup"),
            0,
            1,
            true,
            START,
        ),
    );
    stations.insert(
        via_server::domain::ids::BikeStationId::new("dropoff"),
        BikeStationAvailability::new(
            via_server::domain::ids::BikeStationId::new("dropoff"),
            1,
            0,
            true,
            START,
        ),
    );
    let matrix_calls = Arc::new(AtomicUsize::new(0));
    let augmenter = BikeJourneyAugmenter::new(
        network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(Arc::new(BikeAvailabilitySnapshot::new(START, stations))),
        }),
        Arc::new(CountingMatrixRouter {
            matrix_calls: matrix_calls.clone(),
        }),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    );

    let variants = augmenter
        .augment(JourneySkeleton::from_candidate(&request(), candidate()).unwrap())
        .await;

    assert_eq!(matrix_calls.load(Ordering::Relaxed), 0);
    assert!(variants.iter().all(|variant| variant.is_baseline()));
}

#[tokio::test]
async fn endpoint_matrix_failures_are_isolated_from_boarding_point_shortcuts() {
    let origin = request().origin.coordinate;
    let destination = request().destination.coordinate;
    let network = endpoint_and_boarding_bike_network(origin, destination);
    let skeleton = JourneySkeleton::from_candidate(&request(), candidate()).unwrap();

    for (fail_origin, fail_destination, expected_shortcut) in [
        (true, false, (1, 5)),
        (false, true, (0, 3)),
        (true, true, (1, 3)),
    ] {
        let augmenter = BikeJourneyAugmenter::new(
            network.clone(),
            Arc::new(FixtureAvailabilitySource {
                snapshot: Ok(bike_snapshot(START, false)),
            }),
            Arc::new(EndpointFailureRouter {
                fail_origin,
                fail_destination,
            }),
            BikeAugmentationPolicy {
                now: Some(START),
                ..BikeAugmentationPolicy::default()
            },
        );

        let variants = augmenter.augment(skeleton.clone()).await;

        assert!(variants.iter().any(|variant| {
            variant.path().iter().any(|step| {
                matches!(
                    step.movement,
                    JourneyPathMovement::BikeShare { ref shortcut }
                        if (shortcut.from_anchor, shortcut.to_anchor) == expected_shortcut
                )
            })
        }));
    }
}

#[tokio::test]
async fn pure_endpoint_bike_variant_reconstructs_without_transit_metadata() {
    let transit_network = fixtures::network();
    let origin = near_stop(&transit_network, "F-M1");
    let destination = near_stop(&transit_network, "F-M2-RED");
    let origin_request = RequestCoordinate {
        latitude: origin.latitude(),
        longitude: origin.longitude(),
    };
    let destination_request = RequestCoordinate {
        latitude: destination.latitude(),
        longitude: destination.longitude(),
    };
    let (bike_network, pickup_coordinate, dropoff_coordinate) =
        endpoint_bike_network(origin_request, destination_request);
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let street = FakeStreetRouter::builder()
        .path(
            StreetTravelMode::Walking,
            origin,
            stop_coordinate(&transit_network, "F-M1"),
            path(origin, stop_coordinate(&transit_network, "F-M1"), 60),
        )
        .path(
            StreetTravelMode::Walking,
            stop_coordinate(&transit_network, "F-M2-RED"),
            destination,
            path(
                stop_coordinate(&transit_network, "F-M2-RED"),
                destination,
                60,
            ),
        )
        .path(
            StreetTravelMode::Walking,
            origin,
            pickup,
            path(origin, pickup, 20),
        )
        .path(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            path(pickup, dropoff, 90),
        )
        .path(
            StreetTravelMode::Walking,
            dropoff,
            destination,
            path(dropoff, destination, 25),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(90),
            Metres(600),
        )
        .build();
    let body = fixtures::route_body_with_bike(
        street,
        FixtureRealtime::empty(),
        fixtures::request(origin, destination, &["metro", "bike_share"]),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    )
    .await;
    let bike = body["journey_options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| {
            option["bike_share_uses"] == 1
                && option["legs"]
                    .as_array()
                    .is_some_and(|legs| legs.iter().any(|leg| leg["mode"] == "bicycle"))
        })
        .unwrap_or_else(|| panic!("pure bike option missing: {body}"));

    assert_eq!(bike["transfer_count"], 0);
    assert_eq!(
        bike["legs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|leg| leg["mode"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["walking", "bicycle", "walking"]
    );
    assert!(
        bike["legs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|leg| leg["details"]["via_service_id"].is_null())
    );
}

#[tokio::test]
async fn bike_share_only_uses_the_direct_walking_skeleton() {
    let origin = request().origin.coordinate;
    let destination = request().destination.coordinate;
    let (bike_network, pickup_coordinate, dropoff_coordinate) =
        endpoint_bike_network(origin, destination);
    let origin =
        via_server::street::Coordinate::try_new(origin.latitude, origin.longitude).unwrap();
    let destination =
        via_server::street::Coordinate::try_new(destination.latitude, destination.longitude)
            .unwrap();
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let street = FakeStreetRouter::builder()
        .path(
            StreetTravelMode::Walking,
            origin,
            destination,
            path(origin, destination, 300),
        )
        .path(
            StreetTravelMode::Walking,
            origin,
            pickup,
            path(origin, pickup, 20),
        )
        .path(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            path(pickup, dropoff, 90),
        )
        .path(
            StreetTravelMode::Walking,
            dropoff,
            destination,
            path(dropoff, destination, 25),
        )
        .matrix_leg(
            StreetTravelMode::Walking,
            origin,
            pickup,
            Seconds(20),
            Metres(100),
        )
        .matrix_leg(
            StreetTravelMode::Walking,
            dropoff,
            destination,
            Seconds(25),
            Metres(125),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(90),
            Metres(600),
        )
        .build();
    let body = fixtures::route_body_with_bike(
        street,
        FixtureRealtime::empty(),
        fixtures::request(origin, destination, &["bike_share"]),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    )
    .await;

    let options = body["journey_options"].as_array().unwrap();
    assert!(options.iter().any(|option| {
        option["bike_share_uses"] == 0 && fixtures::leg_modes(option) == vec!["walking"]
    }));
    let bike = options
        .iter()
        .find(|option| option["bike_share_uses"] == 1)
        .expect("bike-share option");
    assert_eq!(bike["transfer_count"], 0);
    assert_eq!(bike["travel_modes"], json!(["walking", "bicycle"]));
    assert_eq!(
        bike["legs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|leg| leg["mode"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["walking", "bicycle", "walking"]
    );
    assert!(
        bike["legs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|leg| leg["details"]["via_service_id"].is_null())
    );
}

#[tokio::test]
async fn bike_variant_reconstructs_with_station_legs_and_bicycle_geometry() {
    let transit_network = fixtures::network();
    let origin = near_stop(&transit_network, "F-M1");
    let destination = near_stop(&transit_network, "F-M2-RED");
    let pickup_coordinate = RequestCoordinate {
        latitude: 25.01,
        longitude: 121.51,
    };
    let dropoff_coordinate = RequestCoordinate {
        latitude: 25.02,
        longitude: 121.52,
    };
    let pickup_id = via_server::domain::ids::BikeStationId::new("pickup");
    let dropoff_id = via_server::domain::ids::BikeStationId::new("dropoff");
    let bike_network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(pickup_id.clone(), "Fixture Pickup", pickup_coordinate),
            BikeStation::new(dropoff_id.clone(), "Fixture Dropoff", dropoff_coordinate),
        ])
        .with_access_links(vec![
            BikeStationAccess::new(
                BoardingPointId::new("F-M1"),
                pickup_id.clone(),
                Some(BikeWalkingAccess::new(20, 100)),
                None,
            ),
            BikeStationAccess::new(
                BoardingPointId::new("F-M2-RED"),
                dropoff_id.clone(),
                None,
                Some(BikeWalkingAccess::new(20, 100)),
            ),
        ])
        .with_revision("fixture-bike"),
    );
    let m1 = stop_coordinate(&transit_network, "F-M1");
    let m2 = stop_coordinate(&transit_network, "F-M2-RED");
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let street = FakeStreetRouter::builder()
        .path(StreetTravelMode::Walking, origin, m1, path(origin, m1, 60))
        .path(
            StreetTravelMode::Walking,
            m2,
            destination,
            path(m2, destination, 60),
        )
        .path(StreetTravelMode::Walking, m1, pickup, path(m1, pickup, 15))
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
            path(dropoff, m2, 15),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(90),
            Metres(600),
        )
        .build();
    let source = Arc::new(FixtureAvailabilitySource {
        snapshot: Ok(bike_snapshot(START, false)),
    });
    let repeated_street = street.clone();
    let repeated_network = bike_network.clone();
    let repeated_source = source.clone();
    let body = fixtures::route_body_with_bike(
        street,
        FixtureRealtime::empty(),
        fixtures::request(origin, destination, &["metro", "bike_share"]),
        bike_network,
        source,
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    )
    .await;
    let options = body["journey_options"].as_array().unwrap();
    let bike = options
        .iter()
        .find(|option| option["bike_share_uses"] == 1)
        .unwrap_or_else(|| panic!("bike option missing: {body}"));
    assert_eq!(bike["travel_modes"], json!(["walking", "bicycle"]));
    assert_eq!(
        bike["legs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|leg| leg["mode"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["walking", "walking", "bicycle", "walking", "walking"]
    );
    assert_eq!(bike["bicycle_seconds"], 90);
    assert_eq!(bike["legs"][2]["details"]["type"], "bike_share");
    assert_eq!(bike["legs"][2]["details"]["pickup_station_id"], "pickup");
    assert_eq!(
        bike["legs"][2]["details"]["dropoff_station_name"],
        "Fixture Dropoff"
    );
    assert_eq!(bike["legs"][2]["from"]["coordinate"]["latitude"], 25.01);
    assert_eq!(bike["legs"][2]["to"]["coordinate"]["longitude"], 121.52);
    assert_eq!(bike["legs"][1]["from"]["kind"], "boarding_point");
    assert_eq!(bike["legs"][1]["to"]["kind"], "bike_station");
    let repeated = fixtures::route_body_with_bike(
        repeated_street,
        FixtureRealtime::empty(),
        fixtures::request(origin, destination, &["bike_share", "metro"]),
        repeated_network,
        repeated_source,
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    )
    .await;
    let repeated_options = repeated["journey_options"].as_array().unwrap();
    let repeated_bike = repeated_options
        .iter()
        .find(|option| option["bike_share_uses"] == 1)
        .unwrap();
    let baseline = options
        .iter()
        .find(|option| option["bike_share_uses"] == 0)
        .unwrap();
    assert_ne!(bike["matching_key"], baseline["matching_key"]);
    assert_eq!(bike["matching_key"], repeated_bike["matching_key"]);
}

#[tokio::test]
async fn bike_connector_becomes_the_transfer_between_retained_transit_legs() {
    let transit_network = fixtures::network();
    let origin = near_stop(&transit_network, "F-M1");
    let destination = near_stop(&transit_network, "F-M3-BLUE");
    let pickup_coordinate = RequestCoordinate {
        latitude: 25.03,
        longitude: 121.53,
    };
    let dropoff_coordinate = RequestCoordinate {
        latitude: 25.04,
        longitude: 121.54,
    };
    let pickup_id = via_server::domain::ids::BikeStationId::new("pickup");
    let dropoff_id = via_server::domain::ids::BikeStationId::new("dropoff");
    let bike_network = Arc::new(
        BikeShareNetwork::new(vec![
            BikeStation::new(pickup_id.clone(), "Transfer Pickup", pickup_coordinate),
            BikeStation::new(dropoff_id.clone(), "Transfer Dropoff", dropoff_coordinate),
        ])
        .with_access_links(vec![
            BikeStationAccess::new(
                BoardingPointId::new("F-M2-RED"),
                pickup_id,
                Some(BikeWalkingAccess::new(15, 75)),
                None,
            ),
            BikeStationAccess::new(
                BoardingPointId::new("F-M2-BLUE"),
                dropoff_id,
                None,
                Some(BikeWalkingAccess::new(15, 75)),
            ),
        ])
        .with_revision("fixture-bike-transfer"),
    );
    let m1 = stop_coordinate(&transit_network, "F-M1");
    let m2_red = stop_coordinate(&transit_network, "F-M2-RED");
    let m2_blue = stop_coordinate(&transit_network, "F-M2-BLUE");
    let m3_blue = stop_coordinate(&transit_network, "F-M3-BLUE");
    let pickup = via_server::street::Coordinate::try_new(
        pickup_coordinate.latitude,
        pickup_coordinate.longitude,
    )
    .unwrap();
    let dropoff = via_server::street::Coordinate::try_new(
        dropoff_coordinate.latitude,
        dropoff_coordinate.longitude,
    )
    .unwrap();
    let street = FakeStreetRouter::builder()
        .path(StreetTravelMode::Walking, origin, m1, path(origin, m1, 60))
        .path(
            StreetTravelMode::Walking,
            m3_blue,
            destination,
            path(m3_blue, destination, 60),
        )
        .path(
            StreetTravelMode::Walking,
            m2_red,
            pickup,
            path(m2_red, pickup, 10),
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
            m2_blue,
            path(dropoff, m2_blue, 10),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            Seconds(60),
            Metres(400),
        )
        .build();
    let body = fixtures::route_body_with_bike(
        street,
        FixtureRealtime::empty(),
        fixtures::request(
            origin,
            destination,
            &["bus", "metro", "tra", "thsr", "bike_share"],
        ),
        bike_network,
        Arc::new(FixtureAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
        }),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    )
    .await;
    let bike = fixtures::option_for_services(&body, &["fixture-mrt-red", "fixture-mrt-blue"]);
    assert_eq!(bike["bike_share_uses"], 1);
    assert_eq!(bike["transfer_count"], 1);
    let transfers = bike["transfers"].as_array().unwrap();
    assert_eq!(transfers.len(), 1);
    let connector_ids = transfers[0]["connector_leg_ids"].as_array().unwrap();
    assert_eq!(connector_ids.len(), 3);
    let leg_by_id = bike["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|leg| (leg["id"].as_str().unwrap(), leg["mode"].as_str().unwrap()))
        .collect::<HashMap<_, _>>();
    assert_eq!(
        connector_ids
            .iter()
            .map(|id| leg_by_id[id.as_str().unwrap()])
            .collect::<Vec<_>>(),
        vec!["walking", "bicycle", "walking"]
    );
}

#[test]
fn anchor_times_are_monotonic_for_multiple_transit_segments_and_transfers() {
    let skeleton = JourneySkeleton::from_candidate(&request(), candidate()).unwrap();

    assert!(
        skeleton
            .anchors()
            .windows(2)
            .all(|window| window[0].reached_at <= window[1].reached_at)
    );
}

#[tokio::test]
async fn bike_dependencies_are_not_queried_without_bike_share_mode() {
    let network = fixtures::network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M2-RED");
    let calls = Arc::new(AtomicUsize::new(0));
    let body = fixtures::route_body_with_bike(
        fixtures::transit_street(&network, origin, "F-M1", destination, "F-M2-RED"),
        FixtureRealtime::empty(),
        fixtures::request(origin, destination, &["bus", "metro", "tra", "thsr"]),
        bike_network().0,
        Arc::new(CountingAvailabilitySource {
            snapshot: Ok(bike_snapshot(START, false)),
            calls: calls.clone(),
        }),
        BikeAugmentationPolicy {
            now: Some(START),
            ..BikeAugmentationPolicy::default()
        },
    )
    .await;

    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(body["journey_options"].is_array());
}
