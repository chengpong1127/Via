use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::{Value, json};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use via_server::{
    domain::bikeshare::BikeShareNetwork,
    domain::ids::TransitPatternId,
    domain::transit_timing::{DepartureError, SnapshotDepartures, TimingSnapshot},
    domain::{
        ids::BoardingPointId,
        transit::{ScheduledStopTime, TransferEdge, TransitNetwork},
    },
    providers::UnavailableBikeAvailabilitySource,
    routing::JourneyPlanningLimits,
    street::{FakeStreetRouter, StreetTravelMode},
};

use super::fixtures::{
    CountingStreet, CountingTiming, FixtureRealtime, leg_modes, near_stop, network,
    option_for_patterns, request, route_body, route_body_with_limits, route_body_with_network,
    route_body_with_sources, route_body_with_street, transit_street,
};
use crate::support::routing::{coordinate, path, stop_coordinate};

fn current_minute_in_taipei() -> OffsetDateTime {
    OffsetDateTime::now_utc()
        .to_offset(UtcOffset::from_hms(8, 0, 0).expect("valid Taipei offset"))
        .replace_second(0)
        .expect("valid second")
        .replace_nanosecond(0)
        .expect("valid nanosecond")
}

fn format_fixture_time(time: OffsetDateTime) -> String {
    time.format(&Rfc3339).expect("timestamp should format")
}

fn red_line_fixture_network() -> TransitNetwork {
    let mut network = network();
    network
        .patterns
        .retain(|pattern| matches!(pattern.id.as_str(), "fixture-mrt-red" | "fixture-mrt-blue"));
    network.trips.retain(|trip| {
        matches!(
            trip.pattern_id.as_str(),
            "fixture-mrt-red" | "fixture-mrt-blue"
        )
    });
    network
        .routes
        .retain(|route| matches!(route.id.as_str(), "fixture-mrt-red" | "fixture-mrt-blue"));
    network.stops.retain(|stop| {
        matches!(
            stop.id.as_str(),
            "F-M1" | "F-M2-RED" | "F-M2-BLUE" | "F-M3-BLUE"
        )
    });
    network.transfer_edges.clear();
    network
}

fn same_line_reboarding_network() -> TransitNetwork {
    let mut network = red_line_fixture_network();
    let red_route = network
        .patterns
        .iter()
        .find(|pattern| pattern.id.as_str() == "fixture-mrt-red")
        .unwrap()
        .route_id
        .clone();
    let continuation = network
        .patterns
        .iter_mut()
        .find(|pattern| pattern.id.as_str() == "fixture-mrt-blue")
        .unwrap();
    continuation.route_id = red_route.clone();
    continuation.stops[0] = BoardingPointId::new("F-M2-RED");
    network.routes.retain(|route| route.id == red_route);
    network
}

fn terminal_transfer_network() -> TransitNetwork {
    let mut network = red_line_fixture_network();
    network
        .trips
        .retain(|trip| trip.pattern_id.as_str() == "fixture-mrt-red");
    network.transfer_edges = vec![TransferEdge::new(
        BoardingPointId::new("F-M2-RED"),
        BoardingPointId::new("F-M2-BLUE"),
        30,
        100,
    )];
    network
}

#[tokio::test]
async fn simple_mrt_uses_the_fixture_schedule_end_to_end() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M2-RED");
    let street = transit_street(&network, origin, "F-M1", destination, "F-M2-RED");
    let body = route_body(
        street,
        FixtureRealtime::empty(),
        request(origin, destination, &["metro"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-mrt-red"]);

    assert_eq!(leg_modes(option), vec!["walking", "transit", "walking"]);
    assert_eq!(option["travel_modes"], json!(["walking", "transit"]));
    assert_eq!(option["transfer_count"], 0);
    assert_eq!(option["legs"][1]["details"]["transit_mode"], "metro");
    assert_eq!(
        option["legs"][1]["details"]["via_boarding_point_id"],
        "F-M1"
    );
    assert_eq!(
        option["legs"][1]["details"]["via_alighting_point_id"],
        "F-M2-RED"
    );
    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:02:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        "2026-08-31T10:07:00+08:00"
    );
}

fn three_stop_red_line_network() -> TransitNetwork {
    let mut network = red_line_fixture_network();
    network
        .patterns
        .retain(|pattern| pattern.id.as_str() == "fixture-mrt-red");
    network
        .trips
        .retain(|trip| trip.pattern_id.as_str() == "fixture-mrt-red");
    let red = &mut network.patterns[0];
    red.stops.push(BoardingPointId::new("F-M3-BLUE"));
    let segment = red.static_profile.segments[0].clone();
    red.static_profile.segments.push(segment);
    for trip in &mut network.trips {
        let next = trip.stop_times.last().unwrap().departure_seconds + 300;
        trip.stop_times.push(ScheduledStopTime::new(next, next));
    }
    network
}

#[tokio::test]
async fn transit_legs_list_intermediate_stops_and_street_guidance_carries_geometry_indexes() {
    let network = three_stop_red_line_network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M3-BLUE");
    let body = route_body_with_network(
        transit_street(&network, origin, "F-M1", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["metro"]),
        network,
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-mrt-red"]);
    let legs = option["legs"].as_array().unwrap();

    assert_eq!(leg_modes(option), vec!["walking", "transit", "walking"]);
    let stops = legs[1]["details"]["intermediate_stops"].as_array().unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0]["via_id"], "F-M2-RED");
    assert_eq!(stops[0]["display_name"], "Fixture MRT Interchange Red");
    assert_ne!(
        legs[1]["geometry"]["points"],
        json!(""),
        "transit geometry should follow its stops"
    );
    for leg in [&legs[0], &legs[2]] {
        let guidance = leg["guidance"].as_array().unwrap();
        assert!(!guidance.is_empty());
        for guidance in guidance {
            assert!(guidance["parameters"]["geometry_index"].is_string());
        }
    }
}

#[tokio::test]
async fn same_line_reboarding_is_rejected_during_corridor_search() {
    let network = same_line_reboarding_network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M3-BLUE");
    let body = route_body_with_network(
        transit_street(&network, origin, "F-M1", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["metro"]),
        network,
    )
    .await;
    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");
    assert!(body.get("journey_options").is_none());
}

#[tokio::test]
async fn terminal_egress_routes_from_the_retained_alighting_anchor() {
    let network = terminal_transfer_network();
    let origin = near_stop(&network, "F-M1");
    let from_transfer = stop_coordinate(&network, "F-M2-RED");
    let egress = stop_coordinate(&network, "F-M2-BLUE");
    let destination = near_stop(&network, "F-M2-BLUE");
    let street = FakeStreetRouter::builder()
        .path(
            StreetTravelMode::Walking,
            from_transfer,
            egress,
            path(from_transfer, egress, 45),
        )
        .path(
            StreetTravelMode::Walking,
            origin,
            stop_coordinate(&network, "F-M1"),
            path(origin, stop_coordinate(&network, "F-M1"), 60),
        )
        .path(
            StreetTravelMode::Walking,
            from_transfer,
            destination,
            path(from_transfer, destination, 120),
        )
        .path(
            StreetTravelMode::Walking,
            egress,
            destination,
            path(egress, destination, 60),
        )
        .build();
    let body = route_body_with_network(
        street,
        FixtureRealtime::empty(),
        request(origin, destination, &["metro"]),
        network,
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-mrt-red"]);
    let legs = option["legs"].as_array().unwrap();

    assert_eq!(leg_modes(option), vec!["walking", "transit", "walking"]);
    assert_eq!(legs[2]["from"]["via_id"], "F-M2-RED");
    assert_eq!(legs[2]["to"]["display_name"], "Fixture destination");
    assert_eq!(legs[2]["duration_seconds"], 120);
    assert!(!legs[2]["geometry"]["points"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn simple_bus_uses_the_earliest_realtime_departure() {
    for modes in [&["bus"][..], &["bus", "metro", "tra", "thsr"][..]] {
        let network = network();
        let origin = near_stop(&network, "F-BUS-A");
        let destination = near_stop(&network, "F-BUS-B");
        let updated_at = current_minute_in_taipei();
        let mut request = request(origin, destination, modes);
        request["departure_setting"]["time"] = json!(format_fixture_time(updated_at));
        let body = route_body(
            transit_street(&network, origin, "F-BUS-A", destination, "F-BUS-B"),
            FixtureRealtime::bus_at(updated_at),
            request,
        )
        .await;
        let option = option_for_patterns(&body, &["fixture-bus-main"]);

        assert_eq!(leg_modes(option), vec!["walking", "transit", "walking"]);
        assert_eq!(option["legs"][1]["details"]["transit_mode"], "bus");
        assert_eq!(
            option["legs"][1]["details"]["via_pattern_id"],
            "fixture-bus-main"
        );
        assert_eq!(
            option["legs"][1]["departure_time"],
            json!(format_fixture_time(updated_at + Duration::minutes(4)))
        );
        assert_eq!(
            option["legs"][1]["arrival_time"],
            json!(format_fixture_time(updated_at + Duration::minutes(14)))
        );
        assert_eq!(option["live_status_summary"]["state"], "fresh");
    }
}

#[tokio::test]
async fn static_bus_schedule_is_used_when_realtime_is_unavailable() {
    let network = network();
    let origin = near_stop(&network, "F-BUS-A");
    let destination = near_stop(&network, "F-BUS-B");
    let body = route_body(
        transit_street(&network, origin, "F-BUS-A", destination, "F-BUS-B"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-bus-main"]);

    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:01:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        "2026-08-31T10:11:00+08:00"
    );
    assert_eq!(option["live_status_summary"]["state"], "not_applicable");
}

#[tokio::test]
async fn arrive_by_rejects_results_after_the_requested_arrival() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M2-RED");
    let mut body = request(origin, destination, &["metro"]);
    body["departure_setting"] = json!({
        "type": "arrive_by",
        "time": "2026-08-31T10:10:00+08:00"
    });
    let body = route_body(
        transit_street(&network, origin, "F-M1", destination, "F-M2-RED"),
        FixtureRealtime::empty(),
        body,
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-mrt-red"]);

    assert!(option["arrival_time"].as_str().unwrap() <= "2026-08-31T10:10:00+08:00");
}

#[tokio::test]
async fn mrt_to_mrt_transfer_reconstructs_one_transfer() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M3-BLUE");
    let body = route_body(
        transit_street(&network, origin, "F-M1", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-mrt-red", "fixture-mrt-blue"]);

    assert_eq!(option["legs"][2]["duration_seconds"], 120);
    assert_eq!(option["transfer_count"], 1);
    assert_eq!(
        leg_modes(option),
        vec!["walking", "transit", "walking", "transit", "walking"]
    );
    assert_eq!(
        option["legs"][1]["details"]["via_alighting_point_id"],
        "F-M2-RED"
    );
    assert_eq!(
        option["legs"][3]["details"]["via_boarding_point_id"],
        "F-M2-BLUE"
    );
    assert!(
        option["legs"][1]["arrival_time"].as_str().unwrap()
            <= option["legs"][3]["departure_time"].as_str().unwrap()
    );
}

#[tokio::test]
async fn zero_transfer_configuration_excludes_interchange_journeys() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M3-BLUE");
    let limits = JourneyPlanningLimits {
        planner: via_server::routing::PlannerLimits {
            max_transfers: 0,
            ..via_server::routing::PlannerLimits::default()
        },
        ..JourneyPlanningLimits::default()
    };
    let body = route_body_with_limits(
        transit_street(&network, origin, "F-M1", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["metro"]),
        limits.clone(),
    )
    .await
    .unwrap();
    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");

    let single_ride_destination = near_stop(&network, "F-M2-RED");
    let single_ride_body = route_body_with_limits(
        transit_street(
            &network,
            origin,
            "F-M1",
            single_ride_destination,
            "F-M2-RED",
        ),
        FixtureRealtime::empty(),
        request(origin, single_ride_destination, &["metro"]),
        limits,
    )
    .await
    .unwrap();
    let option = option_for_patterns(&single_ride_body, &["fixture-mrt-red"]);
    assert_eq!(option["transfer_count"], 0);
}

#[tokio::test]
async fn three_transit_services_reconstruct_with_two_transfers() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M4");
    let body = route_body(
        transit_street(&network, origin, "F-M1", destination, "F-M4"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;
    let option = option_for_patterns(
        &body,
        &["fixture-mrt-red", "fixture-mrt-blue", "fixture-mrt-green"],
    );

    assert_eq!(option["transfer_count"], 2);
    assert_eq!(
        leg_modes(option),
        vec![
            "walking", "transit", "walking", "transit", "walking", "transit", "walking"
        ]
    );
    let transit_legs = option["legs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|leg| leg["mode"] == "transit")
        .collect::<Vec<_>>();
    assert_eq!(transit_legs.len(), 3);
    assert_eq!(
        transit_legs[0]["details"]["via_pattern_id"],
        "fixture-mrt-red"
    );
    assert_eq!(
        transit_legs[1]["details"]["via_pattern_id"],
        "fixture-mrt-blue"
    );
    assert_eq!(
        transit_legs[2]["details"]["via_pattern_id"],
        "fixture-mrt-green"
    );
}

#[tokio::test]
async fn bus_to_mrt_transfer_keeps_the_bus_then_metro_order() {
    let network = network();
    let origin = near_stop(&network, "F-BUS-A");
    let destination = near_stop(&network, "F-M3-BLUE");
    let body = route_body(
        transit_street(&network, origin, "F-BUS-A", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-bus-in", "fixture-mrt-blue"]);

    assert_eq!(option["transfer_count"], 1);
    assert_eq!(
        leg_modes(option),
        vec!["walking", "transit", "walking", "transit", "walking"]
    );
    assert_eq!(
        option["legs"][1]["details"]["via_boarding_point_id"],
        "F-BUS-A"
    );
    assert_eq!(
        option["legs"][1]["details"]["via_alighting_point_id"],
        "F-M2-BUS"
    );
    assert_eq!(
        option["legs"][3]["details"]["via_boarding_point_id"],
        "F-M2-BLUE"
    );
    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:01:00+08:00"
    );
    assert_eq!(
        option["legs"][3]["departure_time"],
        "2026-08-31T10:09:00+08:00"
    );
}

#[tokio::test]
async fn mrt_to_bus_transfer_keeps_the_metro_then_bus_order() {
    let network = network();
    let origin = near_stop(&network, "F-M2-BLUE");
    let destination = near_stop(&network, "F-BUS-B");
    let body = route_body(
        transit_street(&network, origin, "F-M2-BLUE", destination, "F-BUS-B"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-mrt-blue", "fixture-bus-out"]);

    assert_eq!(option["transfer_count"], 1);
    assert_eq!(
        leg_modes(option),
        vec!["walking", "transit", "walking", "transit", "walking"]
    );
    assert_eq!(
        option["legs"][1]["details"]["via_alighting_point_id"],
        "F-M3-BLUE"
    );
    assert_eq!(
        option["legs"][3]["details"]["via_boarding_point_id"],
        "F-M3-BUS"
    );
    assert_eq!(
        option["legs"][3]["departure_time"],
        "2026-08-31T10:17:00+08:00"
    );
}

#[tokio::test]
async fn train_route_uses_the_rail_service_and_stops() {
    let network = network();
    let origin = near_stop(&network, "F-TRA-A");
    let destination = near_stop(&network, "F-TRA-B");
    let body = route_body(
        transit_street(&network, origin, "F-TRA-A", destination, "F-TRA-B"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-tra-local"]);

    assert_eq!(option["legs"][1]["details"]["transit_mode"], "rail");
    assert_eq!(
        option["legs"][1]["details"]["via_boarding_point_id"],
        "F-TRA-A"
    );
    assert_eq!(
        option["legs"][1]["details"]["via_alighting_point_id"],
        "F-TRA-B"
    );
    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:03:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        "2026-08-31T10:18:00+08:00"
    );
}

#[tokio::test]
async fn high_speed_rail_route_uses_the_generic_transit_model() {
    let network = network();
    let origin = near_stop(&network, "F-HSR-A");
    let destination = near_stop(&network, "F-HSR-B");
    let body = route_body(
        transit_street(&network, origin, "F-HSR-A", destination, "F-HSR-B"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-thsr"]);

    assert_eq!(
        option["legs"][1]["details"]["transit_mode"],
        "high_speed_rail"
    );
    assert_eq!(
        option["legs"][1]["details"]["via_pattern_id"],
        "fixture-thsr"
    );
    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:04:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        "2026-08-31T10:12:00+08:00"
    );
}

#[tokio::test]
async fn train_can_transfer_to_local_mrt() {
    let network = network();
    let origin = near_stop(&network, "F-TRA-A");
    let destination = near_stop(&network, "F-M2-RED");
    let body = route_body(
        transit_street(&network, origin, "F-TRA-A", destination, "F-M2-RED"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-tra-local", "fixture-mrt-red"]);

    assert_eq!(option["transfer_count"], 1);
    assert_eq!(
        leg_modes(option),
        vec!["walking", "transit", "walking", "transit", "walking"]
    );
    assert_eq!(option["legs"][1]["details"]["transit_mode"], "rail");
    assert_eq!(option["legs"][3]["details"]["transit_mode"], "metro");
    assert_eq!(option["legs"][2]["duration_seconds"], 120);
    assert_eq!(
        option["legs"][3]["departure_time"],
        "2026-08-31T10:22:00+08:00"
    );
}

#[tokio::test]
async fn direct_walking_is_returned_when_no_transit_access_is_reachable() {
    let origin = coordinate(25.300000, 121.600000);
    let destination = coordinate(25.301000, 121.601000);
    let body = route_body(
        FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                path(origin, destination, 300),
            )
            .build(),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;

    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(leg_modes(&options[0]), vec!["walking"]);
    assert_eq!(options[0]["travel_modes"], json!(["walking"]));
    assert_eq!(options[0]["departure_time"], "2026-08-31T10:00:00+08:00");
    assert_eq!(options[0]["arrival_time"], "2026-08-31T10:05:00+08:00");
}

#[tokio::test]
async fn disconnected_points_return_a_clean_empty_result() {
    let network = network();
    let origin = near_stop(&network, "F-MISS-A");
    let destination = near_stop(&network, "F-MISS-B");
    let body = route_body(
        FakeStreetRouter::builder().build(),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;

    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");
    assert!(body.get("journey_options").is_none());
}

#[tokio::test]
async fn missed_transfer_is_rejected_by_the_two_minute_connection_constraint() {
    let network = network();
    let origin = near_stop(&network, "F-MISS-A");
    let destination = near_stop(&network, "F-MISS-B");
    let body = route_body(
        transit_street(&network, origin, "F-MISS-A", destination, "F-MISS-B"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
    )
    .await;

    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");
}

#[tokio::test]
async fn retained_corridors_without_departures_resolve_direct_walking_once() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let street = Arc::new(CountingStreet {
        inner: FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                stop_coordinate(&transit, "F-M1"),
                path(origin, stop_coordinate(&transit, "F-M1"), 60),
            )
            .path(
                StreetTravelMode::Walking,
                stop_coordinate(&transit, "F-M2-RED"),
                destination,
                path(stop_coordinate(&transit, "F-M2-RED"), destination, 60),
            )
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                path(origin, destination, 300),
            )
            .build(),
        origin,
        destination,
        direct_calls: AtomicUsize::new(0),
        route_failure_at: None,
    });
    let timing = Arc::new(CountingTiming::default());
    let body = route_body_with_sources(
        street.clone(),
        timing.clone(),
        request(origin, destination, &["metro"]),
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
        Arc::new(UnavailableBikeAvailabilitySource),
    )
    .await;

    assert_eq!(timing.calls.load(Ordering::Relaxed), 1);
    assert_eq!(street.direct_calls.load(Ordering::Relaxed), 1);
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(leg_modes(&options[0]), vec!["walking"]);
    assert_eq!(options[0]["duration_seconds"], 300);
}

#[tokio::test]
async fn viable_transit_corridors_compete_with_direct_walking() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let street = Arc::new(CountingStreet {
        inner: FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                stop_coordinate(&transit, "F-M1"),
                path(origin, stop_coordinate(&transit, "F-M1"), 60),
            )
            .path(
                StreetTravelMode::Walking,
                stop_coordinate(&transit, "F-M2-RED"),
                destination,
                path(stop_coordinate(&transit, "F-M2-RED"), destination, 60),
            )
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                path(origin, destination, 300),
            )
            .build(),
        origin,
        destination,
        direct_calls: AtomicUsize::new(0),
        route_failure_at: None,
    });
    let body =
        route_body_with_street(street.clone(), request(origin, destination, &["metro"])).await;

    assert_eq!(street.direct_calls.load(Ordering::Relaxed), 1);
    let options = body["journey_options"].as_array().unwrap();
    let walking = options
        .iter()
        .find(|option| leg_modes(option) == vec!["walking"])
        .unwrap();
    assert_eq!(walking["duration_seconds"], 300);
    assert_eq!(walking["legs"].as_array().unwrap().len(), 1);
    assert!(
        options
            .iter()
            .any(|option| leg_modes(option).contains(&"transit"))
    );
}

#[tokio::test]
async fn no_transit_corridors_skip_timing_and_resolve_direct_walking_once() {
    let origin = coordinate(25.30, 121.60);
    let destination = coordinate(25.301, 121.601);
    let street = Arc::new(CountingStreet {
        inner: FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                path(origin, destination, 300),
            )
            .build(),
        origin,
        destination,
        direct_calls: AtomicUsize::new(0),
        route_failure_at: None,
    });
    let timing = Arc::new(CountingTiming::default());
    let body = route_body_with_sources(
        street.clone(),
        timing.clone(),
        request(origin, destination, &["metro"]),
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
        Arc::new(UnavailableBikeAvailabilitySource),
    )
    .await;

    assert_eq!(timing.calls.load(Ordering::Relaxed), 0);
    assert_eq!(street.direct_calls.load(Ordering::Relaxed), 1);
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(leg_modes(&options[0]), vec!["walking"]);
}

#[tokio::test]
async fn arrive_by_direct_walking_respects_the_latest_arrival() {
    let origin = coordinate(25.30, 121.60);
    let destination = coordinate(25.301, 121.601);
    let latest = "2026-08-31T10:10:00+08:00";
    for (duration, feasible) in [(300, true), (4 * 60 * 60 + 1, false)] {
        let mut body = request(origin, destination, &[]);
        body["departure_setting"] = json!({"type": "arrive_by", "time": latest});
        let result = route_body(
            FakeStreetRouter::builder()
                .path(
                    StreetTravelMode::Walking,
                    origin,
                    destination,
                    path(origin, destination, duration),
                )
                .build(),
            FixtureRealtime::empty(),
            body,
        )
        .await;
        if feasible {
            let options = result["journey_options"].as_array().unwrap();
            assert_eq!(options.len(), 1);
            assert!(options[0]["arrival_time"].as_str().unwrap() <= latest);
            assert_eq!(options[0]["duration_seconds"], duration);
        } else {
            assert_eq!(result["empty"]["code"], "no_feasible_journey_option");
        }
    }
}

#[tokio::test]
async fn arrive_by_leaves_close_to_the_requested_arrival() {
    let origin = coordinate(25.30, 121.60);
    let destination = coordinate(25.301, 121.601);
    let mut body = request(origin, destination, &[]);
    body["departure_setting"] = json!({"type": "arrive_by", "time": "2026-08-31T10:10:00+08:00"});
    let result = route_body(
        FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                path(origin, destination, 300),
            )
            .build(),
        FixtureRealtime::empty(),
        body,
    )
    .await;
    let option = &result["journey_options"][0];

    assert!(option["departure_time"].as_str().unwrap() >= "2026-08-31T09:55:00+08:00");
    assert!(option["arrival_time"].as_str().unwrap() <= "2026-08-31T10:10:00+08:00");
}

#[tokio::test]
async fn arrive_by_walking_replacement_is_checked_after_the_previous_transit_arrival() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M3-BLUE");
    let access = stop_coordinate(&transit, "F-M1");
    let red = stop_coordinate(&transit, "F-M2-RED");
    let blue = stop_coordinate(&transit, "F-M2-BLUE");
    let egress = stop_coordinate(&transit, "F-M3-BLUE");
    for (latest, feasible) in [
        ("2026-08-31T10:14:00+08:00", true),
        ("2026-08-31T10:12:00+08:00", false),
    ] {
        let mut body = request(origin, destination, &["metro"]);
        body["departure_setting"] = json!({"type": "arrive_by", "time": latest});
        let result = route_body(
            FakeStreetRouter::builder()
                .path(StreetTravelMode::Walking, red, blue, path(red, blue, 30))
                .path(
                    StreetTravelMode::Walking,
                    origin,
                    access,
                    path(origin, access, 60),
                )
                .path(
                    StreetTravelMode::Walking,
                    blue,
                    egress,
                    path(blue, egress, 180),
                )
                .path(
                    StreetTravelMode::Walking,
                    egress,
                    destination,
                    path(egress, destination, 60),
                )
                .build(),
            FixtureRealtime::empty(),
            body,
        )
        .await;
        if feasible {
            let option = option_for_patterns(&result, &["fixture-mrt-red"]);
            assert!(option["arrival_time"].as_str().unwrap() <= latest);
            assert!(option["legs"].as_array().unwrap().iter().any(|leg| {
                leg["mode"] == "walking"
                    && leg["from"]["via_id"] == "F-M2-RED"
                    && leg["to"]["via_id"].is_null()
                    && leg["duration_seconds"] == 270
            }));
        } else {
            assert_eq!(result["empty"]["code"], "no_feasible_journey_option");
        }
    }
}

#[tokio::test]
async fn walking_corridor_realization_survives_a_transit_timing_failure() {
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let access = stop_coordinate(&transit, "F-M1");
    let egress = stop_coordinate(&transit, "F-M2-RED");
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert_entry(
        TransitPatternId::new("fixture-mrt-red"),
        0,
        SnapshotDepartures {
            failure: Some(DepartureError::TimedOut),
            ..SnapshotDepartures::default()
        },
    );
    let timing = Arc::new(CountingTiming {
        calls: AtomicUsize::new(0),
        snapshot,
    });
    let street = Arc::new(CountingStreet {
        inner: FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                access,
                path(origin, access, 60),
            )
            .path(
                StreetTravelMode::Walking,
                access,
                egress,
                path(access, egress, 300),
            )
            .path(
                StreetTravelMode::Walking,
                egress,
                destination,
                path(egress, destination, 60),
            )
            .build(),
        origin,
        destination,
        direct_calls: AtomicUsize::new(0),
        route_failure_at: None,
    });
    let body = route_body_with_sources(
        street.clone(),
        timing.clone(),
        request(origin, destination, &["metro"]),
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
        Arc::new(UnavailableBikeAvailabilitySource),
    )
    .await;

    assert_eq!(timing.calls.load(Ordering::Relaxed), 1);
    assert_eq!(street.direct_calls.load(Ordering::Relaxed), 0);
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert!(leg_modes(&options[0]).iter().all(|mode| *mode == "walking"));
    assert_eq!(options[0]["duration_seconds"], 420);
}

#[tokio::test]
async fn matrix_route_mismatch_preserves_a_later_feasible_transit_departure() {
    use time::macros::datetime;
    use via_server::domain::{
        ids::EffectiveTripId,
        transit_timing::{EffectiveStopTime, EffectiveTrip, TimingBasis, TimingReliability},
    };
    use via_server::street::{Metres, Seconds};
    let transit = network();
    let origin = near_stop(&transit, "F-M1");
    let destination = near_stop(&transit, "F-M2-RED");
    let board = stop_coordinate(&transit, "F-M1");
    let alight = stop_coordinate(&transit, "F-M2-RED");
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            origin,
            board,
            Seconds(60),
            Metres(100),
        )
        .path(
            StreetTravelMode::Walking,
            origin,
            board,
            path(origin, board, 180),
        )
        .path(
            StreetTravelMode::Walking,
            alight,
            destination,
            path(alight, destination, 90),
        )
        .build();
    let departure = datetime!(2026-08-31 10:00 +08:00);
    let mut snapshot = TimingSnapshot::default();
    snapshot.insert(
        TransitPatternId::new("fixture-mrt-red"),
        0,
        [("early", 2, 7), ("later", 4, 9)]
            .into_iter()
            .map(|(id, board_minute, alight_minute)| EffectiveTrip {
                id: EffectiveTripId::new(id),
                pattern_id: TransitPatternId::new("fixture-mrt-red"),
                scheduled_trip_id: None,
                stop_times: vec![
                    EffectiveStopTime {
                        stop_id: BoardingPointId::new("F-M1"),
                        arrival_at: departure + Duration::minutes(board_minute),
                        departure_at: departure + Duration::minutes(board_minute),
                    },
                    EffectiveStopTime {
                        stop_id: BoardingPointId::new("F-M2-RED"),
                        arrival_at: departure + Duration::minutes(alight_minute),
                        departure_at: departure + Duration::minutes(alight_minute),
                    },
                ],
                timing_basis: TimingBasis::RealtimePrediction,
                timing_reliability: TimingReliability::Reliable,
                observed_at: None,
                uncertainty_seconds: None,
                evidence: Vec::new(),
            })
            .collect(),
    );
    let body = route_body_with_sources(
        Arc::new(street),
        Arc::new(CountingTiming {
            calls: AtomicUsize::new(0),
            snapshot,
        }),
        request(origin, destination, &["metro"]),
        Arc::new(BikeShareNetwork::new(Vec::new())),
        Arc::new(UnavailableBikeAvailabilitySource),
    )
    .await;
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    let option = &options[0];
    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:04:00+08:00"
    );
    assert_eq!(option["legs"][0]["duration_seconds"], 180);
    assert_eq!(option["legs"][2]["duration_seconds"], 90);
    assert_eq!(option["walking_seconds"], 270);
    assert_eq!(option["duration_seconds"], 630);
    assert_eq!(option["arrival_time"], option["legs"][2]["arrival_time"]);
    assert_eq!(option["arrival_time"], "2026-08-31T10:10:30+08:00");
}

#[tokio::test]
async fn reachable_walking_matrix_cannot_fabricate_unreachable_direct_geometry() {
    use via_server::street::{Metres, Seconds};
    let origin = coordinate(25.0, 121.5);
    let destination = coordinate(25.001, 121.501);
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            origin,
            destination,
            Seconds(60),
            Metres(100),
        )
        .build();
    let timing = Arc::new(CountingTiming::default());
    let body = route_body_with_sources(
        Arc::new(street),
        timing.clone(),
        request(origin, destination, &[]),
        Arc::new(BikeShareNetwork::new(Vec::new())),
        Arc::new(UnavailableBikeAvailabilitySource),
    )
    .await;
    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");
    assert!(body.get("journey_options").is_none());
    assert_eq!(timing.calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn direct_walking_uses_reconstructed_duration_for_all_output_metrics() {
    use via_server::street::{Metres, Seconds};
    let origin = coordinate(25.0, 121.5);
    let destination = coordinate(25.001, 121.501);
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            origin,
            destination,
            Seconds(60),
            Metres(100),
        )
        .path(
            StreetTravelMode::Walking,
            origin,
            destination,
            path(origin, destination, 95),
        )
        .build();
    let body = route_body_with_sources(
        Arc::new(street),
        Arc::new(CountingTiming::default()),
        request(origin, destination, &[]),
        Arc::new(BikeShareNetwork::new(Vec::new())),
        Arc::new(UnavailableBikeAvailabilitySource),
    )
    .await;
    let options = body["journey_options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0]["duration_seconds"], 95);
    assert_eq!(options[0]["walking_seconds"], 95);
    assert_eq!(options[0]["legs"][0]["duration_seconds"], 95);
    assert_eq!(
        options[0]["arrival_time"],
        options[0]["legs"][0]["arrival_time"]
    );
}

#[tokio::test]
async fn reconstructed_walking_duration_must_still_fit_the_query_limit() {
    use via_server::street::{Metres, Seconds};
    let origin = coordinate(25.0, 121.5);
    let destination = coordinate(25.0005, 121.5005);
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            origin,
            destination,
            Seconds(60),
            Metres(100),
        )
        .path(
            StreetTravelMode::Walking,
            origin,
            destination,
            path(origin, destination, 95),
        )
        .build();
    let mut limits = JourneyPlanningLimits::default();
    limits.planner.max_journey_duration_seconds = 70;
    let body = route_body_with_limits(
        street,
        FixtureRealtime::empty(),
        request(origin, destination, &[]),
        limits,
    )
    .await
    .unwrap();
    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");
    assert!(body.get("journey_options").is_none());
}

fn through_train_network() -> TransitNetwork {
    let mut value: Value = serde_json::to_value(network()).unwrap();
    value["stops"].as_array_mut().unwrap().push(json!({
        "id": "F-TRA-C",
        "name": "Fixture TRA Terminal",
        "coordinate": {"latitude": 25.122, "longitude": 121.522}
    }));
    let patterns = value["patterns"].as_array_mut().unwrap();
    patterns.push(json!({
        "id": "fixture-tra-through",
        "route_id": "fixture-tra-local",
        "direction": null,
        "headsign": "Fixture TRA Terminal",
        "stops": ["F-TRA-A", "F-TRA-B", "F-TRA-C"],
        "static_profile": {"segments": [
            {"seconds": 900, "source": "schedule"},
            {"seconds": 600, "source": "schedule"}
        ]}
    }));
    patterns.push(json!({
        "id": "fixture-tra-branch",
        "route_id": "fixture-tra-local",
        "direction": null,
        "headsign": "Fixture TRA Terminal",
        "stops": ["F-TRA-B", "F-TRA-C"],
        "static_profile": {"segments": [{"seconds": 600, "source": "schedule"}]}
    }));
    let trips = value["trips"].as_array_mut().unwrap();
    trips.push(json!({
        "id": "fixture-tra-through-run",
        "pattern_id": "fixture-tra-through",
        "calendar_id": "fixture-monday",
        "stop_times": [
            {"arrival_seconds": 36300, "departure_seconds": 36300},
            {"arrival_seconds": 37200, "departure_seconds": 37380},
            {"arrival_seconds": 37980, "departure_seconds": 37980}
        ]
    }));
    trips.push(json!({
        "id": "fixture-tra-branch-run",
        "pattern_id": "fixture-tra-branch",
        "calendar_id": "fixture-monday",
        "stop_times": [
            {"arrival_seconds": 39000, "departure_seconds": 39000},
            {"arrival_seconds": 39600, "departure_seconds": 39600}
        ]
    }));
    TransitNetwork::from_artifact_json(&value.to_string()).unwrap()
}

#[tokio::test]
async fn staying_on_one_train_is_never_shown_as_two_rides() {
    let network = through_train_network();
    let origin = near_stop(&network, "F-TRA-A");
    let destination = near_stop(&network, "F-TRA-C");
    let body = route_body_with_network(
        transit_street(&network, origin, "F-TRA-A", destination, "F-TRA-C"),
        FixtureRealtime::empty(),
        request(origin, destination, &["tra"]),
        network,
    )
    .await;
    let options = body["journey_options"].as_array().unwrap();

    assert!(!options.is_empty());
    for option in options {
        let rides = option["legs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|leg| leg["mode"] == "transit")
            .count();
        assert_eq!(
            option["transfer_count"],
            rides.saturating_sub(1),
            "{option}"
        );
    }
}
