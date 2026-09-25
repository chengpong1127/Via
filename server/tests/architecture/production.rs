use via_server::domain::{
    ids::{BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId},
    location::RequestCoordinate,
    transit::{
        ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime, ServiceCalendar,
        TransitMode, TransitNetwork, TransitPattern, TransitRoute, TransitStop, TransitTrip,
    },
    transit_runtime::TransitRuntime,
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

    let runtime = TransitRuntime::new(std::sync::Arc::new(network));
    let nearest = runtime.nearest_stops(
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
        vec![stop("point", 25.0, 121.5)],
        vec![pattern],
        vec![trip],
    );
    assert!(network.validate().is_err());
    let network = network
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
        }])
        .with_revisions("static-1", "schedule-1");
    assert!(network.validate().is_ok());
}
