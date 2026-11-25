use super::*;
use crate::{
    routing::corridors::{CorridorAnchor, PlannedMovement},
    street::{Coordinate, Metres, Seconds, StreetPath, StreetTravelMode},
};

type Position = (f64, f64);
type Hop = (Position, Position, u32);

const ORIGIN: (f64, f64) = (25.0, 121.5);
const ORIGIN_MATE: (f64, f64) = (25.0008, 121.5);
const MIDDLE: (f64, f64) = (25.01, 121.5);
const MIDDLE_ALIGHT_MATE: (f64, f64) = (25.0108, 121.5);
const MIDDLE_BOARD_MATE: (f64, f64) = (25.0092, 121.5);
const DESTINATION: (f64, f64) = (25.02, 121.5);
const DESTINATION_MATE: (f64, f64) = (25.0208, 121.5);

fn coordinate((latitude, longitude): (f64, f64)) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

fn named_stop(id: &str, name: &str, position: (f64, f64)) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        name,
        GeoCoordinate {
            latitude: position.0,
            longitude: position.1,
        },
    )
}

fn walk_path(from: (f64, f64), to: (f64, f64), seconds: u32) -> StreetPath {
    StreetPath {
        duration: Seconds(seconds),
        distance: Metres(100),
        geometry: vec![coordinate(from), coordinate(to)],
        maneuvers: Vec::new(),
        attribution: Vec::new(),
    }
}

fn hop_street(hops: &[Hop]) -> MatrixCache {
    let mut router = FakeStreetRouter::builder();
    for &(from, to, seconds) in hops {
        router = router.path(
            StreetTravelMode::Walking,
            coordinate(from),
            coordinate(to),
            walk_path(from, to, seconds),
        );
    }
    MatrixCache::new(Arc::new(router.build()))
}

fn leg(pattern_id: &str, board: &str, alight: &str) -> StaticTransitLeg {
    let StaticCorridorStep::Transit(leg) = static_leg(pattern_id, board, alight) else {
        unreachable!()
    };
    leg
}

fn anchor(id: &str, position: (f64, f64)) -> CorridorAnchor {
    CorridorAnchor {
        kind: JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new(id),
        },
        coordinate: coordinate(position),
    }
}

fn endpoint(kind: JourneyAnchorKind, position: (f64, f64)) -> CorridorAnchor {
    CorridorAnchor {
        kind,
        coordinate: coordinate(position),
    }
}

fn access_walk() -> PlannedMovement {
    PlannedMovement::RetainedWalking {
        kind: WalkingMovementKind::Access,
        duration_seconds: 0,
        distance_metres: 0,
    }
}

fn egress_walk() -> PlannedMovement {
    PlannedMovement::RetainedWalking {
        kind: WalkingMovementKind::Egress,
        duration_seconds: 0,
        distance_metres: 0,
    }
}

fn place_network(patterns: Vec<TransitPattern>, stops: Vec<TransitStop>) -> Arc<TransitRuntime> {
    Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::fixture(
        Time::from_hms(4, 0, 0).unwrap(),
        stops,
        patterns,
        Vec::new(),
    ))))
}

fn single_ride_stops() -> Vec<TransitStop> {
    vec![
        named_stop("origin", "origin", ORIGIN),
        named_stop("o2", "origin", ORIGIN_MATE),
        named_stop("destination", "destination", DESTINATION),
        named_stop("d2", "destination", DESTINATION_MATE),
    ]
}

fn single_ride_corridor() -> Corridor {
    Corridor::new(
        vec![
            endpoint(JourneyAnchorKind::Origin, ORIGIN),
            anchor("origin", ORIGIN),
            anchor("destination", DESTINATION),
            endpoint(JourneyAnchorKind::Destination, DESTINATION),
        ],
        vec![
            access_walk(),
            PlannedMovement::Transit {
                legs: vec![leg("bus-b", "o2", "d2")],
            },
            egress_walk(),
        ],
    )
    .unwrap()
}

async fn ride_place_mate(
    departure_minute: i64,
    arrival_minute: i64,
    hops: &[Hop],
) -> Vec<RealizedJourney> {
    let network = place_network(
        vec![pattern("bus-b", TransitMode::Bus, &["o2", "d2"])],
        single_ride_stops(),
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus-b"),
        0,
        vec![trip(
            "ride",
            "bus-b",
            &["o2", "d2"],
            &[departure_minute, arrival_minute],
        )],
    );
    let query = query(&[TransitMode::Bus], 0, 10);
    evaluate_corridors(
        &network,
        &snapshot,
        &query,
        &hop_street(hops),
        None,
        &[single_ride_corridor()],
    )
    .await
    .unwrap()
    .journeys
}

#[tokio::test]
async fn place_mate_trip_departing_inside_the_boarding_hop_is_not_catchable() {
    let journeys = ride_place_mate(1, 10, &[(ORIGIN, ORIGIN_MATE, 120)]).await;

    assert!(journeys.is_empty());
}

#[tokio::test]
async fn place_mate_trip_departing_after_the_boarding_hop_adds_the_hop_walk() {
    let journeys = ride_place_mate(
        3,
        10,
        &[
            (ORIGIN, ORIGIN_MATE, 120),
            (DESTINATION_MATE, DESTINATION, 0),
        ],
    )
    .await;

    assert_eq!(journeys.len(), 1);
    let timing = journeys[0].evaluate_timing().unwrap();
    assert_eq!(timing.walking_seconds, 120);
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:10 UTC));
}

#[tokio::test]
async fn place_mate_alighting_adds_the_hop_to_the_arrival_at_the_anchor() {
    let journeys = ride_place_mate(
        3,
        10,
        &[
            (ORIGIN, ORIGIN_MATE, 120),
            (DESTINATION_MATE, DESTINATION, 100),
        ],
    )
    .await;

    assert_eq!(journeys.len(), 1);
    let timing = journeys[0].evaluate_timing().unwrap();
    assert_eq!(timing.walking_seconds, 220);
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:11:40 UTC));
    assert_eq!(
        journeys[0].anchors()[2].reached_at,
        datetime!(2026-09-21 10:11:40 UTC)
    );
}

#[tokio::test]
async fn place_mate_leg_without_a_walkable_hop_is_dropped() {
    let journeys = ride_place_mate(3, 10, &[]).await;

    assert!(journeys.is_empty());
}

#[tokio::test]
async fn exact_stop_leg_needs_no_hop() {
    let network = place_network(
        vec![pattern(
            "bus-a",
            TransitMode::Bus,
            &["origin", "destination"],
        )],
        single_ride_stops(),
    );
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus-a"),
        0,
        vec![trip("ride", "bus-a", &["origin", "destination"], &[0, 10])],
    );
    let corridor = Corridor::new(
        vec![
            endpoint(JourneyAnchorKind::Origin, ORIGIN),
            anchor("origin", ORIGIN),
            anchor("destination", DESTINATION),
            endpoint(JourneyAnchorKind::Destination, DESTINATION),
        ],
        vec![
            access_walk(),
            PlannedMovement::Transit {
                legs: vec![leg("bus-a", "origin", "destination")],
            },
            egress_walk(),
        ],
    )
    .unwrap();
    let query = query(&[TransitMode::Bus], 0, 10);

    let journeys = evaluate_corridors(
        &network,
        &snapshot,
        &query,
        &hop_street(&[]),
        None,
        &[corridor],
    )
    .await
    .unwrap()
    .journeys;

    assert_eq!(journeys.len(), 1);
    assert_eq!(journeys[0].evaluate_timing().unwrap().walking_seconds, 0);
}

async fn chained_place_rides(second_departure_minute: i64) -> Vec<RealizedJourney> {
    let network = place_network(
        vec![
            pattern("bus-a", TransitMode::Bus, &["origin", "m2"]),
            pattern("bus-c", TransitMode::Bus, &["m3", "destination"]),
        ],
        vec![
            named_stop("origin", "origin", ORIGIN),
            named_stop("middle", "middle", MIDDLE),
            named_stop("m2", "middle", MIDDLE_ALIGHT_MATE),
            named_stop("m3", "middle", MIDDLE_BOARD_MATE),
            named_stop("destination", "destination", DESTINATION),
        ],
    );
    let corridor = Corridor::new(
        vec![
            endpoint(JourneyAnchorKind::Origin, ORIGIN),
            anchor("origin", ORIGIN),
            anchor("middle", MIDDLE),
            anchor("destination", DESTINATION),
            endpoint(JourneyAnchorKind::Destination, DESTINATION),
        ],
        vec![
            access_walk(),
            PlannedMovement::Transit {
                legs: vec![leg("bus-a", "origin", "m2")],
            },
            PlannedMovement::Transit {
                legs: vec![leg("bus-c", "m3", "destination")],
            },
            egress_walk(),
        ],
    )
    .unwrap();
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("bus-a"),
        0,
        vec![trip("first", "bus-a", &["origin", "m2"], &[1, 5])],
    );
    snapshot.insert(
        TransitPatternId::new("bus-c"),
        0,
        vec![trip(
            "second",
            "bus-c",
            &["m3", "destination"],
            &[second_departure_minute, 20],
        )],
    );
    let query = query(&[TransitMode::Bus], 1, 10);
    evaluate_corridors(
        &network,
        &snapshot,
        &query,
        &hop_street(&[
            (MIDDLE_ALIGHT_MATE, MIDDLE, 100),
            (MIDDLE, MIDDLE_BOARD_MATE, 90),
        ]),
        None,
        &[corridor],
    )
    .await
    .unwrap()
    .journeys
}

#[tokio::test]
async fn chained_places_reject_a_connection_inside_both_hops_and_the_transfer_buffer() {
    let journeys = chained_place_rides(10).await;

    assert!(journeys.is_empty());
}

#[tokio::test]
async fn chained_places_accept_a_connection_after_both_hops_and_the_transfer_buffer() {
    let journeys = chained_place_rides(11).await;

    assert_eq!(journeys.len(), 1);
    let timing = journeys[0].evaluate_timing().unwrap();
    assert_eq!(timing.walking_seconds, 190);
    assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:20 UTC));
}
