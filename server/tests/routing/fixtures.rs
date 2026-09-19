use std::sync::Arc;

use axum::{Json, Router, http::Method, response::IntoResponse, routing::any};
use serde_json::{Value, json};
use via_server::{
    api::PlanningRequest,
    data::load_network_artifact,
    domain::{
        ids::BoardingPointId,
        realtime::FreshnessPolicy,
        transit::{TransferEdge, TransitNetwork},
    },
    providers::{
        service_conditions::{CachedServiceConditionProvider, NullServiceConditionProvider},
        timing::{
            BusDepartureSource, DepartureDispatcher, MetroDepartureSource, ThsrDepartureSource,
            TraDepartureSource, TransitDepartureSource,
        },
    },
    routing::{JourneyAugmenter, RoutingService, TransferIndex, transfers::TransferShortcutIndex},
    street::{Coordinate, FakeStreetRouter},
    tdx::TdxClient,
};

pub(crate) use crate::support::routing::{near_stop, transit_street};
use crate::support::tdx::spawn_tdx_fixture;

const NETWORK_JSON: &str = include_str!("../../fixtures/routing/network.json");

pub(crate) fn network() -> TransitNetwork {
    TransitNetwork::from_artifact_json(NETWORK_JSON).unwrap()
}

fn bus_tdx_payload() -> Value {
    serde_json::from_str(include_str!("../../fixtures/routing/realtime/bus.json")).unwrap()
}

async fn fixture_tdx_client(payload: Value) -> Arc<TdxClient> {
    let app = Router::new().fallback(any(move |method: Method| {
        let payload = payload.clone();
        async move {
            if method == Method::POST {
                Json(json!({"access_token": "fixture-token", "expires_in": 300})).into_response()
            } else {
                Json(payload).into_response()
            }
        }
    }));
    spawn_tdx_fixture(app).await
}

#[derive(Clone, Copy)]
pub(crate) struct FixtureRealtime {
    arrivals_available: bool,
}

impl FixtureRealtime {
    pub(crate) fn empty() -> Self {
        Self {
            arrivals_available: false,
        }
    }

    pub(crate) fn bus() -> Self {
        Self {
            arrivals_available: true,
        }
    }
}

pub(crate) fn request(origin: Coordinate, destination: Coordinate, modes: &[&str]) -> Value {
    json!({
        "origin": {"coordinate": {"latitude": origin.latitude(), "longitude": origin.longitude()}, "display_text": "Fixture origin"},
        "destination": {"coordinate": {"latitude": destination.latitude(), "longitude": destination.longitude()}, "display_text": "Fixture destination"},
        "selected_modes": modes,
        "locale": "en",
        "departure_setting": {"type": "depart_at", "time": "2026-08-31T10:00:00+08:00"}
    })
}

pub(crate) async fn route_body(
    street: FakeStreetRouter,
    realtime: FixtureRealtime,
    body: Value,
) -> Value {
    route_body_inner(street, realtime, body, None).await
}

pub(crate) async fn route_body_with_augmenter(
    street: FakeStreetRouter,
    realtime: FixtureRealtime,
    body: Value,
    augmenter: Arc<dyn JourneyAugmenter>,
) -> Value {
    route_body_inner(street, realtime, body, Some(augmenter)).await
}

async fn route_body_inner(
    street: FakeStreetRouter,
    realtime: FixtureRealtime,
    body: Value,
    augmenter: Option<Arc<dyn JourneyAugmenter>>,
) -> Value {
    let request: PlanningRequest = serde_json::from_value(body).unwrap();
    request.validate().unwrap();
    let network = Arc::new(network());
    let tdx = if realtime.arrivals_available {
        Some(fixture_tdx_client(bus_tdx_payload()).await)
    } else {
        None
    };
    let bus = Arc::new(
        BusDepartureSource::new(network.clone(), tdx)
            .with_realtime_endpoint("feed")
            .with_freshness_policy(FreshnessPolicy {
                version: "fixture-1".into(),
                fresh_through: time::Duration::days(30),
                degraded_through: time::Duration::days(31),
            }),
    );
    let metro = Arc::new(MetroDepartureSource::new(network.clone()));
    let tra = Arc::new(TraDepartureSource::new(network.clone()));
    let thsr = Arc::new(ThsrDepartureSource::new(network.clone()));
    let departure = Arc::new(DepartureDispatcher::new(
        network.clone(),
        vec![
            bus as Arc<dyn TransitDepartureSource>,
            metro as Arc<dyn TransitDepartureSource>,
            tra as Arc<dyn TransitDepartureSource>,
            thsr as Arc<dyn TransitDepartureSource>,
        ],
    ));
    let cached = Arc::new(CachedServiceConditionProvider::new(Arc::new(
        NullServiceConditionProvider,
    )));
    let service =
        RoutingService::new(Arc::new(street), departure, cached).with_transit_network_arc(network);
    let service = match augmenter {
        Some(augmenter) => service.with_journey_augmenter(augmenter),
        None => service,
    };
    serde_json::to_value(service.route(request).await.unwrap()).unwrap()
}

pub(crate) fn option_for_services<'a>(body: &'a Value, services: &[&str]) -> &'a Value {
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

pub(crate) fn leg_modes(option: &Value) -> Vec<&str> {
    option["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|leg| leg["mode"].as_str().unwrap())
        .collect()
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
