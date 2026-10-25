use time::{Duration, macros::datetime};

use crate::domain::ids::BoardingPointId;

use super::*;

#[test]
fn continuous_bike_rental_spans_corridor_anchors_as_one_realized_movement() {
    let departure_at = datetime!(2026-09-21 10:00 UTC);
    let arrival_at = departure_at + Duration::seconds(30);
    let movement = BikeShareMovement::new(
        1,
        3,
        BikeStationId::new("pickup"),
        BikeStationId::new("dropoff"),
        BikeWalkingAccess::new(0, 0),
        BikeRide {
            duration_seconds: 30,
            distance_metres: 100,
        },
        BikeWalkingAccess::new(0, 0),
    )
    .unwrap();
    let anchors = [
        JourneyAnchorKind::Origin,
        JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new("a"),
        },
        JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new("b"),
        },
        JourneyAnchorKind::BoardingPoint {
            id: BoardingPointId::new("c"),
        },
        JourneyAnchorKind::Destination,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, kind)| JourneyAnchor {
        kind,
        reached_at: if index < 3 { departure_at } else { arrival_at },
    })
    .collect();
    let walk = |from_anchor, to_anchor, kind, reached_at| JourneyMovement {
        from_anchor,
        to_anchor,
        departure_at: reached_at,
        arrival_at: reached_at,
        kind: JourneyMovementKind::Walking {
            kind,
            duration_seconds: 0,
            distance_metres: 0,
        },
    };
    let journey = RealizedJourney::from_movements(
        anchors,
        vec![
            walk(0, 1, WalkingMovementKind::Access, departure_at),
            JourneyMovement {
                from_anchor: 1,
                to_anchor: 3,
                departure_at,
                arrival_at,
                kind: JourneyMovementKind::BikeShare { movement },
            },
            walk(3, 4, WalkingMovementKind::Egress, arrival_at),
        ],
        departure_at,
    )
    .unwrap();

    assert_eq!(journey.movements().len(), 3);
    assert!(matches!(
        journey.movements()[1].kind,
        JourneyMovementKind::BikeShare { .. }
    ));
    assert_eq!(journey.movements()[1].from_anchor, 1);
    assert_eq!(journey.movements()[1].to_anchor, 3);
    assert_eq!(
        journey.identity().canonical_signature(),
        "path:v2:baseline:walking:Access:origin:boarding_point:a:0:0|bike:boarding_point:a:boarding_point:c:pickup:dropoff:0:0:30:100:0:0:0:30|baseline:walking:Egress:boarding_point:c:destination:0:0"
    );
    let timing = journey.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, arrival_at);
    assert_eq!(timing.bike_share_uses, 1);
}

#[test]
fn realized_bike_path_needs_no_walking_placeholder() {
    let departure_at = datetime!(2026-09-21 10:00 UTC);
    let movement = BikeShareMovement::new(
        0,
        1,
        BikeStationId::new("pickup"),
        BikeStationId::new("dropoff"),
        BikeWalkingAccess::new(10, 15),
        BikeRide {
            duration_seconds: 30,
            distance_metres: 100,
        },
        BikeWalkingAccess::new(20, 25),
    )
    .unwrap();
    let arrival_at = departure_at + Duration::seconds(60);
    let journey = RealizedJourney::from_movements(
        vec![
            JourneyAnchor {
                kind: JourneyAnchorKind::Origin,
                reached_at: departure_at,
            },
            JourneyAnchor {
                kind: JourneyAnchorKind::Destination,
                reached_at: arrival_at,
            },
        ],
        vec![JourneyMovement {
            from_anchor: 0,
            to_anchor: 1,
            departure_at,
            arrival_at,
            kind: JourneyMovementKind::BikeShare { movement },
        }],
        departure_at,
    )
    .unwrap();
    assert!(matches!(
        journey.movements()[0].kind,
        JourneyMovementKind::BikeShare { .. }
    ));
    let timing = journey.evaluate_timing().unwrap();
    assert_eq!(timing.arrival_at, arrival_at);
    assert_eq!(timing.walking_seconds, 30);
    assert_eq!(timing.bike_share_uses, 1);
    assert!(journey.transit_pattern_sequence().is_empty());
}

#[test]
fn realized_path_rejects_anchor_references_outside_the_corridor() {
    let departure_at = datetime!(2026-09-21 10:00 UTC);
    let result = RealizedJourney::from_movements(
        vec![
            JourneyAnchor {
                kind: JourneyAnchorKind::Origin,
                reached_at: departure_at,
            },
            JourneyAnchor {
                kind: JourneyAnchorKind::Destination,
                reached_at: departure_at,
            },
        ],
        vec![JourneyMovement {
            from_anchor: 0,
            to_anchor: 2,
            departure_at,
            arrival_at: departure_at,
            kind: JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Direct,
                duration_seconds: 0,
                distance_metres: 0,
            },
        }],
        departure_at,
    );
    assert_eq!(
        result.unwrap_err(),
        RealizedJourneyError::InvalidRealizedPath
    );
}
