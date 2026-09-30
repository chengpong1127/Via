use via_server::domain::{
    ids::{BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId},
    location::RequestCoordinate,
    transit::{
        ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime, ServiceCalendar,
        TransitMode, TransitNetwork, TransitNetworkValidationError, TransitPattern, TransitRoute,
        TransitStop, TransitTrip,
    },
};

fn stop(id: &str) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        id,
        RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    )
}

#[test]
fn artifact_validation_requires_revisions_and_accepts_service_day_exceptions() {
    let point = BoardingPointId::new("point");
    let pattern = TransitPattern::new(
        TransitPatternId::new("pattern"),
        TransitRouteId::new("line"),
        None,
        "headsign",
        vec![point.clone(), point.clone()],
        SegmentProfile {
            segments: vec![SegmentTravelTime {
                seconds: 600,
                source: SegmentTimeSource::Schedule,
            }],
        },
    );
    let trip = TransitTrip::new(
        TransitTripId::new("trip"),
        pattern.id.clone(),
        ServiceCalendarId::new("special"),
        vec![
            ScheduledStopTime::new(90_000, 90_000),
            ScheduledStopTime::new(90_600, 90_600),
        ],
    );
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).expect("valid fixture time"),
        vec![stop("point")],
        vec![pattern],
        vec![trip],
    )
    .with_routes(vec![TransitRoute {
        id: TransitRouteId::new("line"),
        mode: TransitMode::Metro,
        label: "line".into(),
    }])
    .with_calendars(vec![ServiceCalendar {
        id: ServiceCalendarId::new("special"),
        weekdays: Vec::new(),
        start_date: None,
        end_date: None,
        added_dates: vec![time::macros::date!(2026 - 09 - 01)],
        removed_dates: Vec::new(),
    }]);
    assert_eq!(
        network.validate(),
        Err(TransitNetworkValidationError::MissingRevision)
    );
    let network = network.with_revisions("static-1", "schedule-1");
    assert!(network.validate().is_ok());
}
