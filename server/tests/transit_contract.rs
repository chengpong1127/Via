use time::{OffsetDateTime, Weekday, macros::datetime};
use via_server::{
    api::{RequestCoordinate, SelectedMode},
    normalization::{BoardingPointId, ScheduledRunId, TransitServiceId},
    transit::{
        ScheduledStopTime, ScheduledTrip, TransitAccess, TransitMode, TransitNetwork, TransitQuery,
        TransitService, TransitStop,
    },
};

fn stop(id: &str, name: &str) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        name,
        RequestCoordinate {
            latitude: 25.04,
            longitude: 121.56,
        },
    )
}

fn trip(id: &str, service_id: &str, times: &[(u32, u32)]) -> ScheduledTrip {
    ScheduledTrip::weekdays(
        ScheduledRunId::new(id),
        TransitServiceId::new(service_id),
        vec![Weekday::Monday],
        times
            .iter()
            .map(|(arrival, departure)| ScheduledStopTime::new(*arrival, *departure))
            .collect(),
    )
}

fn network() -> TransitNetwork {
    let a = BoardingPointId::new("a");
    let b = BoardingPointId::new("b");
    let c = BoardingPointId::new("c");
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![stop("a", "A"), stop("b", "B"), stop("c", "C")],
        vec![
            TransitService::new(
                TransitServiceId::new("bus-1"),
                TransitMode::Bus,
                "1",
                "C",
                vec![a.clone(), b.clone()],
            ),
            TransitService::new(
                TransitServiceId::new("metro-1"),
                TransitMode::Metro,
                "Blue",
                "C",
                vec![b, c],
            ),
        ],
        vec![
            trip(
                "run-bus",
                "bus-1",
                &[(26 * 3600, 26 * 3600), (26 * 3600 + 600, 26 * 3600 + 600)],
            ),
            trip(
                "run-metro",
                "metro-1",
                &[
                    (26 * 3600 + 1_200, 26 * 3600 + 1_200),
                    (26 * 3600 + 1_800, 26 * 3600 + 1_800),
                ],
            ),
        ],
        Vec::new(),
    )
}

#[test]
fn after_midnight_query_uses_previous_service_day_and_can_transfer() {
    let network = network();
    let query_time: OffsetDateTime = datetime!(2026-09-01 01:30 +08:00);
    let access = vec![TransitAccess::new(
        BoardingPointId::new("a"),
        query_time,
        query_time,
    )];

    let itineraries = network.itineraries(
        &access,
        TransitQuery::DepartAfter {
            earliest_departure: query_time,
        },
        &[SelectedMode::Bus, SelectedMode::Metro],
    );

    assert!(
        itineraries
            .iter()
            .any(|itinerary| itinerary.rides.len() == 2)
    );
    assert_eq!(
        itineraries[0].service_day,
        time::macros::date!(2026 - 08 - 31)
    );
}

#[test]
fn selected_modes_are_applied_before_timetable_search() {
    let network = network();
    let query_time: OffsetDateTime = datetime!(2026-09-01 01:30 +08:00);
    let access = vec![TransitAccess::new(
        BoardingPointId::new("a"),
        query_time,
        query_time,
    )];

    let itineraries = network.itineraries(
        &access,
        TransitQuery::DepartAfter {
            earliest_departure: query_time,
        },
        &[SelectedMode::Bus],
    );

    assert!(!itineraries.is_empty());
    assert!(itineraries.iter().all(|itinerary| {
        itinerary
            .rides
            .iter()
            .all(|ride| ride.mode == TransitMode::Bus)
    }));
}
