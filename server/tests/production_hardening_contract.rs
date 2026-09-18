use via_server::domain::{
    ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
    location::RequestCoordinate,
    transit::{
        ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService, TransitStop,
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
        ScheduledTripId::new("trip"),
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
    );
    assert!(network.validate().is_err());
    let network = network.with_revisions("static-1", "schedule-1");
    assert!(network.validate().is_ok());
}
