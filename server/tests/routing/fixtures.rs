use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;

use axum::{Json, Router, http::Method, response::IntoResponse, routing::any};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    api::PlanningRequest,
    domain::{
        bikeshare::BikeShareNetwork,
        ids::BoardingPointId,
        journey::PlanningError,
        realtime::FreshnessPolicy,
        transit::{TransferEdge, TransitNetwork},
        transit_runtime::TransitRuntime,
        transit_timing::{DepartureQuery, TimingSnapshot, TimingSnapshotLoader},
    },
    providers::{
        BikeAvailabilitySource, UnavailableBikeAvailabilitySource,
        timing::{
            BusDepartureSource, DepartureResolver, MetroDepartureSource, TdxProviderBindings,
            ThsrDepartureSource, TraDepartureSource, TransitDepartureSource,
        },
    },
    routing::{JourneyPlanningLimits, RoutingService, transfers::TransferShortcutIndex},
    street::{
        Coordinate, FakeStreetRouter, MatrixQuery, MatrixResult, RouteQuery, StreetPath,
        StreetRouter, StreetRoutingError,
    },
    tdx::TdxClient,
};

pub(crate) use crate::support::routing::{near_stop, transit_street};
use crate::support::tdx::{failing_tdx_client, spawn_tdx_fixture};

const NETWORK_JSON: &str = include_str!("../../fixtures/routing/network.json");
const BINDINGS_JSON: &str = include_str!("../../fixtures/routing/provider-bindings.json");

pub(crate) fn network() -> TransitNetwork {
    TransitNetwork::from_artifact_json(NETWORK_JSON).unwrap()
}

#[derive(Default)]
pub(crate) struct CountingTiming {
    pub(crate) calls: AtomicUsize,
    pub(crate) snapshot: TimingSnapshot,
}

#[async_trait]
impl TimingSnapshotLoader for CountingTiming {
    async fn load_snapshot(&self, _queries: Vec<DepartureQuery>) -> TimingSnapshot {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.snapshot.clone()
    }
}

pub(crate) struct CountingStreet {
    pub(crate) inner: FakeStreetRouter,
    pub(crate) origin: Coordinate,
    pub(crate) destination: Coordinate,
    pub(crate) direct_calls: AtomicUsize,
    pub(crate) route_failure_at: Option<(Coordinate, Coordinate)>,
}

#[async_trait]
impl StreetRouter for CountingStreet {
    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        self.inner.matrix(query).await
    }

    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        if query.origin == self.origin && query.destination == self.destination {
            self.direct_calls.fetch_add(1, Ordering::Relaxed);
        }
        if self.route_failure_at == Some((query.origin, query.destination)) {
            return Err(StreetRoutingError::Timeout);
        }
        self.inner.route(query).await
    }
}

pub(crate) async fn route_body_with_sources(
    street: Arc<dyn StreetRouter>,
    timing: Arc<dyn TimingSnapshotLoader>,
    body: Value,
    bike_network: Arc<BikeShareNetwork>,
    availability: Arc<dyn BikeAvailabilitySource>,
) -> Value {
    let request: PlanningRequest = serde_json::from_value(body).unwrap();
    request.validate().unwrap();
    let service = RoutingService::new(
        street,
        Arc::new(TransitRuntime::new(Arc::new(network()))),
        timing,
        bike_network,
        availability,
        JourneyPlanningLimits::default(),
    );
    serde_json::to_value(service.plan(request).await.unwrap()).unwrap()
}

fn bus_tdx_payload(updated_at: OffsetDateTime) -> Value {
    let mut payload: Value =
        serde_json::from_str(include_str!("../../fixtures/routing/realtime/bus.json")).unwrap();
    let updated_at = updated_at
        .format(&Rfc3339)
        .expect("timestamp should format");
    for record in payload.as_array_mut().expect("fixture should be an array") {
        record["UpdateTime"] = Value::String(updated_at.clone());
    }
    payload
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
    bus_updated_at: Option<OffsetDateTime>,
}

impl FixtureRealtime {
    pub(crate) fn empty() -> Self {
        Self {
            bus_updated_at: None,
        }
    }

    pub(crate) fn bus_at(updated_at: OffsetDateTime) -> Self {
        Self {
            bus_updated_at: Some(updated_at),
        }
    }
}

pub(crate) fn request(origin: Coordinate, destination: Coordinate, modes: &[&str]) -> Value {
    json!({
        "origin": {"coordinate": {"latitude": origin.latitude(), "longitude": origin.longitude()}, "display_text": "Fixture origin"},
        "destination": {"coordinate": {"latitude": destination.latitude(), "longitude": destination.longitude()}, "display_text": "Fixture destination"},
        "selected_modes": modes,
        "departure_setting": {"type": "depart_at", "time": "2026-08-31T10:00:00+08:00"}
    })
}

pub(crate) async fn route_body(
    street: FakeStreetRouter,
    realtime: FixtureRealtime,
    body: Value,
) -> Value {
    route_body_inner(
        Arc::new(street),
        realtime,
        body,
        None,
        JourneyPlanningLimits::default(),
        network(),
    )
    .await
    .unwrap()
}

pub(crate) async fn route_body_with_network(
    street: FakeStreetRouter,
    realtime: FixtureRealtime,
    body: Value,
    network: TransitNetwork,
) -> Value {
    route_body_inner(
        Arc::new(street),
        realtime,
        body,
        None,
        JourneyPlanningLimits::default(),
        network,
    )
    .await
    .unwrap()
}

pub(crate) async fn route_body_with_bike(
    street: FakeStreetRouter,
    realtime: FixtureRealtime,
    body: Value,
    bike_network: Arc<BikeShareNetwork>,
    availability_source: Arc<dyn BikeAvailabilitySource>,
) -> Value {
    route_body_inner(
        Arc::new(street),
        realtime,
        body,
        Some((bike_network, availability_source)),
        JourneyPlanningLimits::default(),
        network(),
    )
    .await
    .unwrap()
}

pub(crate) async fn route_body_with_limits(
    street: FakeStreetRouter,
    realtime: FixtureRealtime,
    body: Value,
    limits: JourneyPlanningLimits,
) -> Result<Value, PlanningError> {
    route_body_inner(Arc::new(street), realtime, body, None, limits, network()).await
}

pub(crate) async fn route_body_with_street(street: Arc<dyn StreetRouter>, body: Value) -> Value {
    route_body_inner(
        street,
        FixtureRealtime::empty(),
        body,
        None,
        JourneyPlanningLimits::default(),
        network(),
    )
    .await
    .unwrap()
}

async fn route_body_inner(
    street: Arc<dyn StreetRouter>,
    realtime: FixtureRealtime,
    body: Value,
    bike: Option<(Arc<BikeShareNetwork>, Arc<dyn BikeAvailabilitySource>)>,
    limits: JourneyPlanningLimits,
    transit_network: TransitNetwork,
) -> Result<Value, PlanningError> {
    let request: PlanningRequest = serde_json::from_value(body).unwrap();
    request.validate().unwrap();
    let network = Arc::new(TransitRuntime::new(Arc::new(transit_network)));
    let bindings: TdxProviderBindings = serde_json::from_str(BINDINGS_JSON).unwrap();
    let tdx = if let Some(updated_at) = realtime.bus_updated_at {
        Some(fixture_tdx_client(bus_tdx_payload(updated_at)).await)
    } else {
        None
    }
    .unwrap_or_else(failing_tdx_client);
    let bus = Arc::new(
        BusDepartureSource::new(network.clone(), tdx.clone())
            .with_bindings(Arc::new(bindings))
            .with_realtime_endpoint("feed")
            .with_freshness_policy(FreshnessPolicy {
                fresh_through: time::Duration::days(30),
                degraded_through: time::Duration::days(31),
            }),
    );
    let metro = Arc::new(MetroDepartureSource::new(network.clone(), tdx.clone()));
    let tra = Arc::new(TraDepartureSource::new(network.clone(), tdx));
    let thsr = Arc::new(ThsrDepartureSource::new(network.clone()));
    let departure = Arc::new(DepartureResolver::new(
        network.clone(),
        vec![
            bus as Arc<dyn TransitDepartureSource>,
            metro as Arc<dyn TransitDepartureSource>,
            tra as Arc<dyn TransitDepartureSource>,
            thsr as Arc<dyn TransitDepartureSource>,
        ],
    ));
    let service_street = street;
    let (bike_network, availability_source) = bike.unwrap_or_else(|| {
        (
            Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture-disabled")),
            Arc::new(UnavailableBikeAvailabilitySource) as Arc<dyn BikeAvailabilitySource>,
        )
    });
    let service = RoutingService::new(
        service_street,
        network,
        departure,
        bike_network,
        availability_source,
        limits,
    );
    Ok(serde_json::to_value(service.plan(request).await?).unwrap())
}

pub(crate) fn option_for_patterns<'a>(body: &'a Value, services: &[&str]) -> &'a Value {
    body["journey_options"]
        .as_array()
        .unwrap_or_else(|| panic!("response has no journey options: {body}"))
        .iter()
        .find(|option| {
            option["legs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|leg| leg["details"]["via_pattern_id"].as_str())
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
    let runtime = TransitRuntime::new(Arc::new(network.clone()));
    let transfers = TransferShortcutIndex::from_network(&network);

    assert_eq!(network.stops.len(), 18);
    assert_eq!(network.patterns.len(), 10);
    assert_eq!(network.trips.len(), 17);
    assert_eq!(
        network
            .patterns
            .iter()
            .filter(|pattern| runtime.route_of(pattern).mode
                == via_server::domain::transit::TransitMode::Metro)
            .count(),
        3
    );
    assert_eq!(network.transfer_edges.len(), 12);
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
    let fixture = std::fs::read_to_string(artifact_path).unwrap();
    assert_eq!(
        TransitNetwork::from_artifact_json(&fixture).unwrap(),
        network
    );
}

#[test]
fn transfer_fixture_rejects_unknown_zero_duration_and_self_transfers() {
    let mut unknown = network();
    unknown.transfer_edges.push(TransferEdge::new(
        BoardingPointId::new("F-M1"),
        BoardingPointId::new("F-UNKNOWN"),
        120,
        100,
    ));
    assert!(unknown.validate().is_err());

    let mut zero_duration = network();
    zero_duration.transfer_edges.push(TransferEdge::new(
        BoardingPointId::new("F-M2-RED"),
        BoardingPointId::new("F-M2-BLUE"),
        0,
        100,
    ));
    assert!(zero_duration.validate().is_err());

    let mut self_transfer = network();
    self_transfer.transfer_edges.push(TransferEdge::new(
        BoardingPointId::new("F-M1"),
        BoardingPointId::new("F-M1"),
        120,
        100,
    ));
    assert!(self_transfer.validate().is_err());
}
