use time::{OffsetDateTime, Weekday, macros::datetime};
use via_server::{
    api::{JourneyMode, RequestCoordinate},
    normalization::{BoardingPointId, ScheduledRunId, TransitServiceId},
    transit::{
        ScheduledStopTime, ScheduledTrip, TransferEdge, TransitAccess, TransitMode, TransitNetwork,
        TransitQuery, TransitService, TransitStop,
    },
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
        Vec::new(),
    );

    let nearest = network.nearest_stops(
        RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
        2,
    );
    assert_eq!(nearest[0].id.as_str(), "near");
    assert_eq!(nearest[1].id.as_str(), "middle");
}

#[test]
fn explicit_transfer_edges_connect_distinct_boarding_points() {
    let a = BoardingPointId::new("a");
    let b = BoardingPointId::new("b");
    let c = BoardingPointId::new("c");
    let d = BoardingPointId::new("d");
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).expect("valid fixture time"),
        vec![
            stop("a", 25.00, 121.50),
            stop("b", 25.01, 121.51),
            stop("c", 25.011, 121.511),
            stop("d", 25.02, 121.52),
        ],
        vec![
            TransitService::new(
                TransitServiceId::new("bus"),
                TransitMode::Bus,
                "1",
                "d",
                vec![a.clone(), b.clone()],
            ),
            TransitService::new(
                TransitServiceId::new("metro"),
                TransitMode::Metro,
                "blue",
                "d",
                vec![c.clone(), d],
            ),
        ],
        vec![
            ScheduledTrip::weekdays(
                ScheduledRunId::new("bus-run"),
                TransitServiceId::new("bus"),
                vec![Weekday::Monday],
                vec![
                    ScheduledStopTime::new(36_000, 36_000),
                    ScheduledStopTime::new(36_600, 36_600),
                ],
            ),
            ScheduledTrip::weekdays(
                ScheduledRunId::new("metro-run"),
                TransitServiceId::new("metro"),
                vec![Weekday::Monday],
                vec![
                    ScheduledStopTime::new(36_720, 36_720),
                    ScheduledStopTime::new(37_200, 37_200),
                ],
            ),
        ],
        Vec::new(),
    )
    .with_transfer_edges(vec![TransferEdge::new(b, c, 60)]);

    let query_time: OffsetDateTime = datetime!(2026-08-31 09:00 +08:00);
    let itineraries = network.itineraries(
        &[TransitAccess::new(a, query_time, query_time)],
        TransitQuery::DepartAfter {
            earliest_departure: query_time,
        },
        &[JourneyMode::Transit],
    );
    assert!(
        itineraries
            .iter()
            .any(|itinerary| itinerary.rides.len() == 2)
    );
}

#[test]
fn artifact_validation_requires_revisions_and_accepts_service_day_exceptions() {
    let point = BoardingPointId::new("point");
    let service = TransitService::new(
        TransitServiceId::new("service"),
        TransitMode::Metro,
        "line",
        "headsign",
        vec![point.clone(), point.clone()],
    );
    let trip = ScheduledTrip::weekdays(
        ScheduledRunId::new("trip"),
        service.id.clone(),
        Vec::new(),
        vec![
            ScheduledStopTime::new(90_000, 90_000),
            ScheduledStopTime::new(90_600, 90_600),
        ],
    )
    .with_service_day_exceptions(vec![time::macros::date!(2026 - 09 - 01)], Vec::new());
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).expect("valid fixture time"),
        vec![stop("point", 25.0, 121.5)],
        vec![service],
        vec![trip],
        Vec::new(),
    );
    assert!(network.validate().is_err());
    let network = network.with_revisions("static-1", "schedule-1");
    assert!(network.validate().is_ok());
}
