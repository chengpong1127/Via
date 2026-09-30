use std::sync::Arc;

use time::{Weekday, macros::datetime};
use via_server::{
    domain::{
        ids::{
            BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
        },
        transit::{
            ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime,
            ServiceCalendar, TransitFrequency, TransitMode, TransitNetwork,
            TransitNetworkValidationError, TransitPattern, TransitRoute, TransitStop, TransitTrip,
        },
        transit_runtime::TransitRuntime,
        transit_timing::{DepartureQuery, TimingBasis, TimingReliability},
    },
    providers::timing::{DepartureDispatcher, MetroDepartureSource, TransitDepartureSource},
};

fn network(id: &str, first: &str, second: &str) -> TransitNetwork {
    let route_id = TransitRouteId::new("metro:line");
    TransitNetwork::new(
        time::macros::time!(4:00),
        [first, second]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                TransitStop::new(
                    BoardingPointId::new(id),
                    id,
                    via_server::api::GeoCoordinate {
                        latitude: 25.0 + index as f64 * 0.001,
                        longitude: 121.5,
                    },
                )
            })
            .collect(),
        vec![TransitPattern::new(
            TransitPatternId::new(id),
            route_id.clone(),
            Some(0),
            second,
            vec![BoardingPointId::new(first), BoardingPointId::new(second)],
            SegmentProfile {
                segments: vec![SegmentTravelTime {
                    seconds: 240,
                    source: SegmentTimeSource::Schedule,
                }],
            },
        )],
        Vec::new(),
    )
    .with_routes(vec![TransitRoute {
        id: route_id,
        mode: TransitMode::Metro,
        label: "Metro".into(),
    }])
    .with_calendars(vec![ServiceCalendar {
        id: ServiceCalendarId::new("daily"),
        weekdays: vec![
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
            Weekday::Saturday,
            Weekday::Sunday,
        ],
        start_date: None,
        end_date: None,
        added_dates: Vec::new(),
        removed_dates: Vec::new(),
    }])
    .with_revisions("static", "schedule")
}

fn trip(id: &str, pattern_id: &str, departure: u32) -> TransitTrip {
    TransitTrip::new(
        TransitTripId::new(id),
        TransitPatternId::new(pattern_id),
        ServiceCalendarId::new("daily"),
        vec![
            ScheduledStopTime::new(departure, departure),
            ScheduledStopTime::new(departure + 240, departure + 240),
        ],
    )
}

fn resolver(network: TransitNetwork) -> DepartureDispatcher {
    let network = Arc::new(TransitRuntime::new(Arc::new(network)));
    let metro = Arc::new(MetroDepartureSource::new(network.clone()));
    DepartureDispatcher::new(network, vec![metro as Arc<dyn TransitDepartureSource>])
}

#[tokio::test]
async fn metro_static_timing_materializes_taipei_main_to_taipei_101() {
    let mut network = network("metro:example", "TRTC-BL12", "TRTC-R03");
    network.trips = vec![
        trip("first", "metro:example", 28_800),
        trip("second", "metro:example", 29_100),
    ];
    network.validate().unwrap();
    let departures = resolver(network)
        .next_departures(DepartureQuery::new(
            TransitPatternId::new("metro:example"),
            BoardingPointId::new("TRTC-BL12"),
            datetime!(2026-09-15 07:55 +08:00),
            2,
        ))
        .await
        .unwrap();
    assert_eq!(departures.len(), 2);
    assert_eq!(departures[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(
        departures[0].stop_times[0].departure_at,
        datetime!(2026-09-15 08:00 +08:00)
    );
    assert_eq!(
        departures[0].stop_times[1].arrival_at,
        datetime!(2026-09-15 08:04 +08:00)
    );
    assert_eq!(
        departures[1].stop_times[0].departure_at,
        datetime!(2026-09-15 08:05 +08:00)
    );
}

#[tokio::test]
async fn metro_frequency_timing_remains_static_fallback() {
    let mut network = network("metro:BL::direction=0", "TRTC-BL12", "TRTC-BL23");
    network.frequencies = vec![TransitFrequency {
        id: TransitTripId::new("frequency"),
        pattern_id: TransitPatternId::new("metro:BL::direction=0"),
        calendar_id: ServiceCalendarId::new("daily"),
        start_seconds: 28_800,
        end_seconds: 29_400,
        headway_seconds: 300,
        exact_times: false,
        template_stop_times: vec![
            ScheduledStopTime::new(28_800, 28_800),
            ScheduledStopTime::new(29_040, 29_040),
        ],
    }];
    let departures = resolver(network)
        .next_departures(DepartureQuery::new(
            TransitPatternId::new("metro:BL::direction=0"),
            BoardingPointId::new("TRTC-BL12"),
            datetime!(2026-09-15 07:55 +08:00),
            2,
        ))
        .await
        .unwrap();
    assert_eq!(departures.len(), 2);
    assert!(
        departures
            .iter()
            .all(|d| d.timing_basis == TimingBasis::FrequencyEstimate
                && d.timing_reliability == TimingReliability::Fallback)
    );
}

#[tokio::test]
async fn metro_static_snapshot_window_crosses_the_service_day_boundary() {
    let mut network = network("metro:boundary", "TRTC-A", "TRTC-B");
    network.trips = vec![
        trip("before-boundary", "metro:boundary", 27 * 3600 + 55 * 60),
        trip("after-boundary", "metro:boundary", 4 * 3600 + 10 * 60),
    ];
    let departures = resolver(network)
        .next_departures(
            DepartureQuery::at_position(
                TransitPatternId::new("metro:boundary"),
                BoardingPointId::new("TRTC-A"),
                0,
                datetime!(2026-08-31 03:50 +08:00),
                10,
            )
            .until(datetime!(2026-08-31 04:30 +08:00)),
        )
        .await
        .unwrap();
    assert_eq!(departures.len(), 2);
    assert_eq!(
        departures[0].stop_times[0].departure_at,
        datetime!(2026-08-31 03:55 +08:00)
    );
    assert_eq!(
        departures[1].stop_times[0].departure_at,
        datetime!(2026-08-31 04:10 +08:00)
    );
}

#[test]
fn metro_rejects_a_profile_with_the_wrong_shape() {
    let mut network = network("metro:R::direction=0", "TRTC-R03", "TRTC-R10");
    network.patterns[0].static_profile.segments.clear();
    assert!(matches!(
        network.validate(),
        Err(TransitNetworkValidationError::InvalidPatternProfile(_))
    ));
}
