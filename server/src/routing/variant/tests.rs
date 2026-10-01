use time::{Duration, macros::datetime};

use crate::{
    domain::{ids::BoardingPointId, transit_timing::TimingReliability},
    routing::{AccessPoint, EgressPoint, JourneySegment, RealizedJourney},
};

use super::*;

#[test]
fn shortcut_replaces_a_contiguous_interval_of_baseline_movements() {
    let departure_at = datetime!(2026-09-21 10:00 UTC);
    let a = BoardingPointId::new("a");
    let b = BoardingPointId::new("b");
    let c = BoardingPointId::new("c");
    let candidate = RealizedJourney {
        access: AccessPoint::new(a.clone(), departure_at, departure_at, 0, 0),
        egress: EgressPoint::new(c.clone(), 0, 0),
        segments: vec![
            JourneySegment::WalkingTransfer {
                shortcut: crate::routing::transfers::TransferShortcut::new(
                    a.clone(),
                    b.clone(),
                    60,
                    100,
                ),
            },
            JourneySegment::WalkingTransfer {
                shortcut: crate::routing::transfers::TransferShortcut::new(b, c, 60, 100),
            },
        ],
        departure_time: departure_at,
        arrival_time: departure_at + Duration::seconds(120),
        walking_seconds: 120,
        transfer_count: 0,
        uncertainty_seconds: None,
        evidence: Vec::new(),
        timing_bases: Vec::new(),
        timing_reliability: TimingReliability::Reliable,
    };
    let baseline = JourneyVariant::baseline(JourneySkeleton::from_candidate(candidate).unwrap());
    assert_eq!(
        baseline.identity().canonical_signature(),
        "access:a:0|transfer:a:b:60|transfer:b:c:60|egress:c:0"
    );
    let shortcut = BikeShortcut::new(
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

    let variant = JourneyVariant::with_shortcut(&baseline, shortcut).unwrap();

    assert_eq!(
        variant.identity().canonical_signature(),
        "path:v2:baseline:walking:Access:origin:boarding_point:a:0:0|bike:boarding_point:a:boarding_point:c:pickup:dropoff:0:0:30:100:0:0:0:30|baseline:walking:Egress:boarding_point:c:destination:0:0"
    );

    assert_eq!(variant.path().len(), 3);
    assert!(variant.path().iter().any(|step| {
        step.from_anchor == 1
            && step.to_anchor == 3
            && matches!(step.movement, JourneyPathMovement::BikeShare { .. })
    }));
    assert_eq!(
        variant.evaluate_timing().unwrap().arrival_at,
        departure_at + Duration::seconds(30)
    );
}
