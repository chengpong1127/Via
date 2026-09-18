use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use serde_json::{Value, json};
use via_server::{
    api::PlanningRequest,
    data::load_network_artifact,
    domain::{
        ids::{BoardingPointId, TransitServiceId},
        realtime::{NormalizedRealtimeValue, RealtimeValue},
        transit::{TransferEdge, TransitNetwork, TransitStop},
    },
    providers::{
        CompositeDepartureResolver, MetroDepartureResolver, RealtimeDepartureResolver,
        realtime::{ArrivalQuery, CachedRealtimeProvider, RealtimeError, RealtimeProvider},
    },
    routing::TransferIndex,
    routing::{RoutingService, transfers::TransferShortcutIndex},
    street::{
        Coordinate, FakeStreetRouter, Maneuver, Metres, Seconds, StreetPath, StreetTravelMode,
    },
};

const NETWORK_JSON: &str = include_str!("../fixtures/routing/network.json");
fn network() -> TransitNetwork {
    TransitNetwork::from_artifact_json(NETWORK_JSON).unwrap()
}

const BUS_REALTIME_JSON: &str = include_str!("../fixtures/routing/realtime/bus.json");

#[derive(Clone, Default)]
struct FixtureRealtime {
    values: Arc<Vec<NormalizedRealtimeValue>>,
    arrivals_available: bool,
}

impl FixtureRealtime {
    fn empty() -> Self {
        Self::default()
    }

    fn bus() -> Self {
        Self {
            values: Arc::new(serde_json::from_str(BUS_REALTIME_JSON).unwrap()),
            arrivals_available: true,
        }
    }
}

#[async_trait]
impl RealtimeProvider for FixtureRealtime {
    async fn arrival_estimates(
        &self,
        query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        if !self.arrivals_available {
            return Ok(Vec::new());
        }
        Ok(self
            .values
            .iter()
            .filter(|value| {
                let RealtimeValue::ArrivalEstimate(arrival) = &value.value else {
                    return false;
                };
                query
                    .transit_service_ids
                    .contains(&arrival.transit_service_id)
                    && query
                        .boarding_point_ids
                        .contains(&arrival.boarding_point_id)
            })
            .cloned()
            .collect())
    }

    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        Ok(Vec::new())
    }
}

fn coordinate(latitude: f64, longitude: f64) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

fn stop_coordinate(network: &TransitNetwork, id: &str) -> Coordinate {
    let stop: &TransitStop = network.stop(&BoardingPointId::new(id)).unwrap();
    coordinate(stop.coordinate.latitude, stop.coordinate.longitude)
}

fn near_stop(network: &TransitNetwork, id: &str) -> Coordinate {
    let stop = stop_coordinate(network, id);
    coordinate(stop.latitude() + 0.0001, stop.longitude() + 0.0001)
}

fn path(from: Coordinate, to: Coordinate, duration: u32) -> StreetPath {
    StreetPath {
        duration: Seconds(duration),
        distance: Metres(duration * 5),
        geometry: vec![from, to],
        maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
        attribution: Vec::new(),
    }
}

fn transit_street(
    network: &TransitNetwork,
    origin: Coordinate,
    access_stop: &str,
    destination: Coordinate,
    egress_stop: &str,
) -> FakeStreetRouter {
    let access = stop_coordinate(network, access_stop);
    let egress = stop_coordinate(network, egress_stop);
    FakeStreetRouter::builder()
        .path(
            StreetTravelMode::Walking,
            origin,
            access,
            path(origin, access, 60),
        )
        .path(
            StreetTravelMode::Walking,
            egress,
            destination,
            path(egress, destination, 60),
        )
        .build()
}

fn request(origin: Coordinate, destination: Coordinate, modes: &[&str]) -> Value {
    json!({
        "origin": {"coordinate": {"latitude": origin.latitude(), "longitude": origin.longitude()}, "display_text": "Fixture origin"},
        "destination": {"coordinate": {"latitude": destination.latitude(), "longitude": destination.longitude()}, "display_text": "Fixture destination"},
        "selected_modes": modes,
        "locale": "en",
        "departure_setting": {"type": "depart_at", "time": "2026-08-31T10:00:00+08:00"}
    })
}

async fn route_body(street: FakeStreetRouter, realtime: FixtureRealtime, body: Value) -> Value {
    let request: PlanningRequest = serde_json::from_value(body).unwrap();
    request.validate().unwrap();
    let network = Arc::new(network());
    let cached = Arc::new(CachedRealtimeProvider::with_ttl(
        Arc::new(realtime),
        StdDuration::from_secs(30),
    ));
    let metro = Arc::new(MetroDepartureResolver::new(network.clone()));
    let departure = Arc::new(
        CompositeDepartureResolver::with_schedule_fallback(
            network.clone(),
            Arc::new(RealtimeDepartureResolver::new(
                cached.clone(),
                network.clone(),
            )),
        )
        .with_adapter("metro_schedule", metro.clone())
        .with_adapter("metro_frequency", metro),
    );
    let service =
        RoutingService::new(Arc::new(street), departure, cached).with_transit_network_arc(network);
    serde_json::to_value(service.route(request).await.unwrap()).unwrap()
}

fn option_for_services<'a>(body: &'a Value, services: &[&str]) -> &'a Value {
    body["journey_options"]
        .as_array()
        .unwrap_or_else(|| panic!("response has no journey options: {body}"))
        .iter()
        .find(|option| {
            option["legs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|leg| leg["details"]["via_service_id"].as_str())
                .eq(services.iter().copied())
        })
        .unwrap_or_else(|| panic!("missing service sequence {services:?}: {body}"))
}

fn leg_modes(option: &Value) -> Vec<&str> {
    option["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|leg| leg["mode"].as_str().unwrap())
        .collect()
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
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-mrt-red"]);

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
async fn simple_bus_uses_the_earliest_realtime_departure() {
    let network = network();
    let origin = near_stop(&network, "F-BUS-A");
    let destination = near_stop(&network, "F-BUS-B");
    let body = route_body(
        transit_street(&network, origin, "F-BUS-A", destination, "F-BUS-B"),
        FixtureRealtime::bus(),
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-bus-main"]);

    assert_eq!(leg_modes(option), vec!["walking", "transit", "walking"]);
    assert_eq!(option["legs"][1]["details"]["transit_mode"], "bus");
    assert_eq!(
        option["legs"][1]["details"]["via_service_id"],
        "fixture-bus-main"
    );
    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:04:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        "2026-08-31T10:14:00+08:00"
    );
    assert_eq!(option["legs"][1]["scheduled_departure_time"], Value::Null);
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
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-bus-main"]);

    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:01:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        "2026-08-31T10:11:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["scheduled_departure_time"],
        "2026-08-31T10:01:00+08:00"
    );
    assert_eq!(option["live_status_summary"]["state"], "not_applicable");
}

#[tokio::test]
async fn mrt_to_mrt_transfer_reconstructs_one_transfer() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M3-BLUE");
    let body = route_body(
        transit_street(&network, origin, "F-M1", destination, "F-M3-BLUE"),
        FixtureRealtime::empty(),
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-mrt-red", "fixture-mrt-blue"]);

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
async fn three_transit_services_reconstruct_with_two_transfers() {
    let network = network();
    let origin = near_stop(&network, "F-M1");
    let destination = near_stop(&network, "F-M4");
    let body = route_body(
        transit_street(&network, origin, "F-M1", destination, "F-M4"),
        FixtureRealtime::empty(),
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(
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
        transit_legs[0]["details"]["via_service_id"],
        "fixture-mrt-red"
    );
    assert_eq!(
        transit_legs[1]["details"]["via_service_id"],
        "fixture-mrt-blue"
    );
    assert_eq!(
        transit_legs[2]["details"]["via_service_id"],
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
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-bus-in", "fixture-mrt-blue"]);

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
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-mrt-blue", "fixture-bus-out"]);

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
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-tra-local"]);

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
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-thsr"]);

    assert_eq!(
        option["legs"][1]["details"]["transit_mode"],
        "high_speed_rail"
    );
    assert_eq!(
        option["legs"][1]["details"]["via_service_id"],
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
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-tra-local", "fixture-mrt-red"]);

    assert_eq!(option["transfer_count"], 1);
    assert_eq!(
        leg_modes(option),
        vec!["walking", "transit", "walking", "transit", "walking"]
    );
    assert_eq!(option["legs"][1]["details"]["transit_mode"], "rail");
    assert_eq!(option["legs"][3]["details"]["transit_mode"], "metro");
    assert_eq!(option["legs"][2]["duration_seconds"], 120);
    assert_eq!(option["transfers"].as_array().unwrap().len(), 1);
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
        request(origin, destination, &["transit"]),
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
        request(origin, destination, &["transit"]),
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
        request(origin, destination, &["transit"]),
    )
    .await;

    assert_eq!(body["empty"]["code"], "no_feasible_journey_option");
}

#[tokio::test]
async fn competing_realtime_bus_departures_choose_the_first_usable_one() {
    let network = network();
    let origin = near_stop(&network, "F-BUS-A");
    let destination = near_stop(&network, "F-BUS-B");
    let body = route_body(
        transit_street(&network, origin, "F-BUS-A", destination, "F-BUS-B"),
        FixtureRealtime::bus(),
        request(origin, destination, &["transit"]),
    )
    .await;
    let option = option_for_services(&body, &["fixture-bus-main"]);

    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:04:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["arrival_time"],
        "2026-08-31T10:14:00+08:00"
    );
}

#[test]
fn routing_fixture_artifacts_are_small_valid_and_bidirectional() {
    let network = network();
    let transfers = TransferShortcutIndex::from_network(&network);

    assert_eq!(network.stops.len(), 18);
    assert_eq!(network.services.len(), 10);
    assert_eq!(network.trips.len(), 8);
    assert_eq!(network.metro_timings.len(), 3);
    assert_eq!(transfers.len(), 12);
    assert!(network.validate().is_ok());
    assert!(
        transfers
            .transfers_from(&BoardingPointId::new("F-TRA-B"))
            .iter()
            .any(|shortcut| shortcut.to == BoardingPointId::new("F-M1"))
    );
    assert!(
        transfers
            .transfers_from(&BoardingPointId::new("F-M1"))
            .iter()
            .any(|shortcut| shortcut.to == BoardingPointId::new("F-TRA-B"))
    );

    let artifact_path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/routing/network.json");
    assert_eq!(load_network_artifact(&artifact_path).unwrap(), network);
}

#[test]
fn transfer_fixture_rejects_unknown_zero_duration_and_self_transfers() {
    let mut unknown = network();
    unknown.transfer_edges.push(TransferEdge::new(
        BoardingPointId::new("F-M1"),
        BoardingPointId::new("F-UNKNOWN"),
        120,
    ));
    assert!(unknown.validate().is_err());

    let mut zero_duration = network();
    zero_duration.transfer_edges.push(TransferEdge::new(
        BoardingPointId::new("F-M2-RED"),
        BoardingPointId::new("F-M2-BLUE"),
        0,
    ));
    assert!(zero_duration.validate().is_err());

    let mut self_transfer = network();
    self_transfer.transfer_edges.push(TransferEdge::new(
        BoardingPointId::new("F-M1"),
        BoardingPointId::new("F-M1"),
        120,
    ));
    assert!(self_transfer.validate().is_err());
}
