use std::sync::Arc;

use time::{Duration, OffsetDateTime, macros::datetime};
use via_server::{
    api::{DepartureSetting, PlanningRequest, RequestCoordinate, SelectedPlace},
    domain::{
        ids::{BoardingPointId, EffectiveTripId, TransitServiceId},
        journey::JourneyMode,
        transit_timing::{EffectiveStopTime, EffectiveTrip, TimingBasis, TimingReliability},
    },
    routing::{
        AccessPoint, BaselineJourneyAugmenter, EgressPoint, JourneyAnchorKind, JourneyAugmenter,
        JourneyCandidate, JourneyMovementKind, JourneySegment, JourneySkeleton, TransferShortcut,
    },
};

use super::fixtures::{self, FixtureRealtime};
use crate::support::routing::near_stop;

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
        selected_modes: vec![JourneyMode::Transit],
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
    assert!(
        matches!(movements[0].kind, JourneyMovementKind::Walking { kind, duration_seconds: 30, distance_metres: 150 } if kind.is_access())
    );
    assert!(matches!(
        movements[1].kind,
        JourneyMovementKind::Transit { segment_index: 0 }
    ));
    assert!(
        matches!(movements[2].kind, JourneyMovementKind::Walking { kind, duration_seconds: 120, .. } if kind.is_transfer())
    );
    assert!(matches!(
        movements[3].kind,
        JourneyMovementKind::Transit { segment_index: 2 }
    ));
    assert!(
        matches!(movements[4].kind, JourneyMovementKind::Walking { kind, duration_seconds: 45, distance_metres: 225 } if kind.is_egress())
    );
    assert_eq!(
        movements
            .iter()
            .map(|movement| (movement.from_anchor, movement.to_anchor))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)]
    );
}

#[test]
fn baseline_augmenter_returns_one_equivalent_baseline_variant() {
    let candidate = candidate();
    let signature = candidate.signature();
    let skeleton = JourneySkeleton::from_candidate(&request(), candidate).unwrap();
    let variants = BaselineJourneyAugmenter.augment(skeleton);

    assert_eq!(variants.len(), 1);
    assert_eq!(variants[0].skeleton().movements().len(), 5);
    assert!(variants[0].is_baseline());
    assert_eq!(variants[0].baseline_signature(), signature);
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
async fn baseline_augmentation_does_not_change_route_results() {
    let network = fixtures::network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M2-RED");
    let body = fixtures::request(origin, destination, &["transit"]);

    let default_result = fixtures::route_body(
        fixtures::transit_street(&network, origin, "F-M1", destination, "F-M2-RED"),
        FixtureRealtime::empty(),
        body.clone(),
    )
    .await;
    let baseline_result = fixtures::route_body_with_augmenter(
        fixtures::transit_street(&network, origin, "F-M1", destination, "F-M2-RED"),
        FixtureRealtime::empty(),
        body,
        Arc::new(BaselineJourneyAugmenter),
    )
    .await;

    // Request IDs and generated timestamps are intentionally fresh per call;
    // the returned route options must be identical.
    assert_eq!(
        default_result["journey_options"],
        baseline_result["journey_options"]
    );
    assert_eq!(default_result["warnings"], baseline_result["warnings"]);
}
