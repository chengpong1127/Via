use serde_json::json;
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use via_server::{
    domain::journey::PlanningError,
    domain::{
        ids::BoardingPointId,
        transit::{TransferEdge, TransitNetwork},
    },
    routing::RoutingLimits,
    street::{FakeStreetRouter, StreetTravelMode},
};

use super::fixtures::{
    FixtureRealtime, leg_modes, near_stop, network, option_for_patterns, request, route_body,
    route_body_with_limits, route_body_with_network, transit_street,
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

#[tokio::test]
async fn same_line_reboarding_counts_a_transfer_between_effective_trips() {
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
    let option = option_for_patterns(&body, &["fixture-mrt-red", "fixture-mrt-blue"]);

    assert_eq!(option["transfer_count"], 1);
    assert_eq!(option["legs"][1]["details"]["service_label"], "R");
    assert_eq!(option["legs"][2]["details"]["service_label"], "R");
}

#[tokio::test]
async fn terminal_surface_transfer_and_egress_are_one_routed_walk() {
    let network = terminal_transfer_network();
    let origin = near_stop(&network, "F-M1");
    let from_transfer = stop_coordinate(&network, "F-M2-RED");
    let egress = stop_coordinate(&network, "F-M2-BLUE");
    let destination = near_stop(&network, "F-M2-BLUE");
    let street = FakeStreetRouter::builder()
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
    let network = network();
    let origin = near_stop(&network, "F-BUS-A");
    let destination = near_stop(&network, "F-BUS-B");
    let updated_at = current_minute_in_taipei();
    let mut request = request(origin, destination, &["bus"]);
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
    let limits = RoutingLimits {
        planner: via_server::routing::PlannerLimits {
            max_transfers: 0,
            ..via_server::routing::PlannerLimits::default()
        },
        ..RoutingLimits::default()
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
async fn phase2_safety_limit_is_returned_as_a_planning_error() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M3-BLUE");
    let limits = RoutingLimits {
        planner: via_server::routing::PlannerLimits {
            max_phase2_expansions: 1,
            ..via_server::routing::PlannerLimits::default()
        },
        ..RoutingLimits::default()
    };

    let result = route_body_with_limits(
        transit_street(&network, origin, "F-M1", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
        limits,
    )
    .await;

    assert_eq!(result.unwrap_err(), PlanningError::Internal);
}

#[tokio::test]
async fn phase1_safety_limit_is_not_reported_as_an_empty_result() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M3-BLUE");
    let limits = RoutingLimits {
        planner: via_server::routing::PlannerLimits {
            max_phase1_expansions: 1,
            ..via_server::routing::PlannerLimits::default()
        },
        ..RoutingLimits::default()
    };

    let result = route_body_with_limits(
        transit_street(&network, origin, "F-M1", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["bus", "metro", "tra", "thsr"]),
        limits,
    )
    .await;

    assert_eq!(result.unwrap_err(), PlanningError::Internal);
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
async fn competing_realtime_bus_departures_choose_the_first_usable_one() {
    let network = network();
    let origin = near_stop(&network, "F-BUS-A");
    let destination = near_stop(&network, "F-BUS-B");
    let updated_at = current_minute_in_taipei();
    let mut request = request(origin, destination, &["bus", "metro", "tra", "thsr"]);
    request["departure_setting"]["time"] = json!(format_fixture_time(updated_at));
    let body = route_body(
        transit_street(&network, origin, "F-BUS-A", destination, "F-BUS-B"),
        FixtureRealtime::bus_at(updated_at),
        request,
    )
    .await;
    let option = option_for_patterns(&body, &["fixture-bus-main"]);

    assert_eq!(
        option["legs"][1]["departure_time"],
        json!(format_fixture_time(updated_at + Duration::minutes(4)))
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        json!(format_fixture_time(updated_at + Duration::minutes(14)))
    );
}
