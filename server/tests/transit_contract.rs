use time::{OffsetDateTime, Weekday, macros::datetime};
use via_server::domain::{
    ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
    journey::JourneyMode,
    location::RequestCoordinate,
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
        ScheduledTripId::new(id),
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
        &[JourneyMode::Transit],
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
fn transit_selection_uses_one_generic_path_for_bus_and_metro() {
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
        &[JourneyMode::Transit],
    );

    assert!(!itineraries.is_empty());
    assert!(itineraries.iter().all(|itinerary| {
        itinerary
            .rides
            .iter()
            .all(|ride| matches!(ride.transit_mode, TransitMode::Bus | TransitMode::Metro))
    }));
    assert!(
        itineraries
            .iter()
            .flat_map(|itinerary| &itinerary.rides)
            .any(|ride| ride.transit_mode == TransitMode::Bus)
    );
    assert!(
        itineraries
            .iter()
            .flat_map(|itinerary| &itinerary.rides)
            .any(|ride| ride.transit_mode == TransitMode::Metro)
    );
}

#[test]
fn rail_service_participates_in_generic_transit_search() {
    let mut network = network();
    network.services.push(TransitService::new(
        TransitServiceId::new("rail-1"),
        TransitMode::Rail,
        "Coastal Rail",
        "C",
        vec![BoardingPointId::new("a"), BoardingPointId::new("c")],
    ));
    network.trips.push(trip(
        "run-rail",
        "rail-1",
        &[
            (26 * 3600 + 300, 26 * 3600 + 300),
            (26 * 3600 + 900, 26 * 3600 + 900),
        ],
    ));
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
        &[JourneyMode::Transit],
    );

    assert!(itineraries.iter().any(|itinerary| {
        itinerary
            .rides
            .iter()
            .any(|ride| ride.transit_mode == TransitMode::Rail)
    }));
}

#[test]
fn high_speed_rail_uses_the_same_generic_transit_search() {
    let mut network = network();
    network.services.push(TransitService::new(
        TransitServiceId::new("hsr-1"),
        TransitMode::HighSpeedRail,
        "High Speed Rail",
        "C",
        vec![BoardingPointId::new("a"), BoardingPointId::new("c")],
    ));
    network.trips.push(trip(
        "run-hsr",
        "hsr-1",
        &[
            (26 * 3600 + 420, 26 * 3600 + 420),
            (26 * 3600 + 1_020, 26 * 3600 + 1_020),
        ],
    ));
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
        &[JourneyMode::Transit],
    );

    assert!(itineraries.iter().any(|itinerary| {
        itinerary
            .rides
            .iter()
            .any(|ride| ride.transit_mode == TransitMode::HighSpeedRail)
    }));
}

#[test]
fn bus_topology_remains_discoverable_when_representative_trip_is_not_active_today() {
    let network = network();
    let query_time: OffsetDateTime = datetime!(2026-09-01 10:00 +08:00);
    let access = vec![TransitAccess::new(
        BoardingPointId::new("a"),
        query_time,
        query_time,
    )];

    let itineraries = network.itineraries(
        &access,
        TransitQuery::ArriveBy {
            latest_arrival: query_time,
        },
        &[JourneyMode::Transit],
    );

    assert!(itineraries.iter().any(|itinerary| {
        itinerary
            .rides
            .iter()
            .any(|ride| ride.transit_mode == TransitMode::Bus)
    }));
}
