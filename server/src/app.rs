use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use time::{OffsetDateTime, UtcOffset};

use crate::{
    api::{API_VERSION, PlanningRequest, SEMANTIC_CATALOG_VERSION, new_request_id, taipei_offset},
    observability::OperationalMetrics,
    planner::{JourneyPlanningService, PlanningError},
    refresh::{RefreshCoordinator, RefreshOutcome},
    snapshot::{InMemorySnapshotStore, NetworkSnapshot, SnapshotStore},
    street::{
        GraphRevision, ProviderCapabilities, ProviderCategory, ProviderError,
        StreetRoutingProvider, StreetRoutingSession,
    },
    transit::TransitNetwork,
};

#[derive(Clone)]
struct AppState {
    street_routing: StreetRoutingSession,
    journey_planning: JourneyPlanningService,
    refresh: Option<RefreshEndpoint>,
    snapshots: Arc<dyn SnapshotStore>,
    transit_network: Arc<TransitNetwork>,
    metrics: OperationalMetrics,
}

#[derive(Clone)]
struct RefreshEndpoint {
    coordinator: RefreshCoordinator,
    token: String,
}

pub async fn build_router(
    provider: Arc<dyn StreetRoutingProvider>,
) -> Result<Router, ProviderError> {
    let now = OffsetDateTime::now_utc()
        .to_offset(UtcOffset::from_hms(8, 0, 0).expect("Taipei UTC offset is valid"));
    let snapshots = Arc::new(InMemorySnapshotStore::new(NetworkSnapshot {
        id: "development-snapshot".into(),
        created_at: now,
        live_cutoff: now,
        static_network_revision: "development-static".into(),
        schedule_revision: "development-schedule".into(),
        freshness_policy_version: "1".into(),
        publication_version: 1,
        feed_evidence: Vec::new(),
        live_status: Vec::new(),
    }));
    build_router_with_store(provider, snapshots).await
}

pub async fn build_router_with_store(
    provider: Arc<dyn StreetRoutingProvider>,
    snapshots: Arc<dyn SnapshotStore>,
) -> Result<Router, ProviderError> {
    build_router_with_services(provider, snapshots, None, None).await
}

pub async fn build_router_with_services(
    provider: Arc<dyn StreetRoutingProvider>,
    snapshots: Arc<dyn SnapshotStore>,
    refresh_coordinator: Option<RefreshCoordinator>,
    refresh_token: Option<String>,
) -> Result<Router, ProviderError> {
    build_router_with_services_and_network(
        provider,
        snapshots,
        refresh_coordinator,
        refresh_token,
        TransitNetwork::default(),
    )
    .await
}

pub async fn build_router_with_services_and_network(
    provider: Arc<dyn StreetRoutingProvider>,
    snapshots: Arc<dyn SnapshotStore>,
    refresh_coordinator: Option<RefreshCoordinator>,
    refresh_token: Option<String>,
    transit_network: TransitNetwork,
) -> Result<Router, ProviderError> {
    let street_routing = StreetRoutingSession::start(provider).await?;
    let journey_planning = JourneyPlanningService::new(street_routing.clone(), snapshots.clone())
        .with_transit_network(transit_network.clone());
    let refresh_endpoint = refresh_coordinator
        .zip(refresh_token)
        .map(|(coordinator, token)| RefreshEndpoint { coordinator, token });
    Ok(Router::new()
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness))
        .route("/metrics", get(metrics))
        .route("/health/street-routing", get(street_routing_health))
        .route("/openapi.json", get(openapi))
        .route("/v1/journey-plans", post(journey_plan))
        .route("/internal/v1/refresh", post(refresh))
        .with_state(AppState {
            street_routing,
            journey_planning,
            refresh: refresh_endpoint,
            snapshots,
            transit_network: Arc::new(transit_network),
            metrics: OperationalMetrics::default(),
        })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(http_timeout)))
}

async fn http_timeout(request: axum::http::Request<Body>, next: Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(10), next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(StatusCode::REQUEST_TIMEOUT, "request_timeout", None),
    }
}

async fn liveness() -> Response {
    no_store((StatusCode::OK, Json(serde_json::json!({ "alive": true }))))
}

async fn metrics(State(state): State<AppState>) -> Response {
    no_store((
        StatusCode::OK,
        [
            ("content-type", "text/plain; version=0.0.4"),
            ("cache-control", "no-store"),
        ],
        state.metrics.prometheus(),
    ))
}

#[derive(Serialize)]
struct ReadinessResponse {
    ready: bool,
    street_routing: bool,
    network_snapshot: bool,
    transit_artifact: bool,
}

async fn readiness(State(state): State<AppState>) -> Response {
    let street_routing = state
        .street_routing
        .current_status()
        .await
        .is_ok_and(|status| {
            status.healthy
                && status.graph_revision == state.street_routing.pinned_status().graph_revision
                && status.capabilities.supports(ProviderCapabilities::REQUIRED)
        });
    let snapshot = state.snapshots.pin_current().await.ok();
    let network_snapshot = snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.publication_version > 0 && !snapshot.id.trim().is_empty());
    let transit_artifact = state.transit_network.validate().is_ok()
        && snapshot.as_ref().is_some_and(|snapshot| {
            state.transit_network.matches_snapshot(
                &snapshot.static_network_revision,
                &snapshot.schedule_revision,
            )
        });
    let body = ReadinessResponse {
        ready: street_routing && network_snapshot && transit_artifact,
        street_routing,
        network_snapshot,
        transit_artifact,
    };
    no_store((
        if body.ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    ))
}

async fn openapi() -> Response {
    let document: serde_json::Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("checked-in OpenAPI is valid");
    no_store((StatusCode::OK, Json(document)))
}

#[derive(Serialize)]
struct HealthResponse {
    healthy: bool,
    provider: ProviderCategory,
    graph_revision: GraphRevision,
    pinned_graph_revision: GraphRevision,
    capabilities: ProviderCapabilities,
}

async fn street_routing_health(State(state): State<AppState>) -> Response {
    let pinned = state.street_routing.pinned_status();
    match state.street_routing.current_status().await {
        Ok(current) => {
            let revision_matches = current.graph_revision == pinned.graph_revision;
            let body = HealthResponse {
                healthy: current.healthy && revision_matches,
                provider: current.category,
                graph_revision: current.graph_revision,
                pinned_graph_revision: pinned.graph_revision.clone(),
                capabilities: current.capabilities,
            };
            no_store((
                if body.healthy {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                Json(body),
            ))
        }
        Err(_) => no_store((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                healthy: false,
                provider: pinned.category,
                graph_revision: pinned.graph_revision.clone(),
                pinned_graph_revision: pinned.graph_revision.clone(),
                capabilities: pinned.capabilities,
            }),
        )),
    }
}

async fn journey_plan(
    State(state): State<AppState>,
    request: Result<Json<PlanningRequest>, JsonRejection>,
) -> Response {
    let request = match request {
        Ok(Json(request)) => request,
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "invalid_request", Some("/")),
    };
    if let Err(error) = request.validate() {
        return error_response(StatusCode::BAD_REQUEST, error.code(), Some(error.field()));
    }
    match state.journey_planning.plan(request).await {
        Ok(outcome) => {
            match &outcome {
                crate::planner::PlanningOutcome::Options(result) => state
                    .metrics
                    .record_planning_success(result.journey_options.len()),
                crate::planner::PlanningOutcome::Empty(_) => {
                    state.metrics.record_planning_success(0)
                }
            }
            no_store((StatusCode::OK, Json(outcome)))
        }
        Err(PlanningError::SnapshotUnavailable | PlanningError::TemporarilyUnavailable) => {
            state.metrics.record_planning_failure();
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                None,
            )
        }
        Err(PlanningError::Internal) => {
            state.metrics.record_planning_failure();
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "planner_failure", None)
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshRequest {
    refresh_key: String,
}

async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<RefreshRequest>, JsonRejection>,
) -> Response {
    let Some(endpoint) = state.refresh else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "refresh_unavailable", None);
    };
    let authorized = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| secure_eq(provided.as_bytes(), endpoint.token.as_bytes()));
    if !authorized {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized_refresh", None);
    }
    let request = match request {
        Ok(Json(request)) if valid_refresh_key(&request.refresh_key) => request,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_refresh_key",
                Some("/refresh_key"),
            );
        }
    };
    match endpoint
        .coordinator
        .run(
            &request.refresh_key,
            OffsetDateTime::now_utc().to_offset(taipei_offset()),
        )
        .await
    {
        Ok(outcome @ RefreshOutcome::Published { .. })
        | Ok(outcome @ RefreshOutcome::AlreadyHandled) => no_store((StatusCode::OK, Json(outcome))),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "refresh_publication_failed",
            None,
        ),
    }
}

fn valid_refresh_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}

fn secure_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(left.get(index).copied().unwrap_or_default())
            ^ usize::from(right.get(index).copied().unwrap_or_default());
    }
    difference == 0
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    api_version: &'static str,
    semantic_catalog_version: &'static str,
    request_id: String,
    error: ErrorObject<'a>,
}

#[derive(Serialize)]
struct ErrorObject<'a> {
    code: &'a str,
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'a str>,
    parameters: serde_json::Value,
}

fn error_response(status: StatusCode, code: &str, field: Option<&str>) -> Response {
    no_store((
        status,
        Json(ErrorEnvelope {
            api_version: API_VERSION,
            semantic_catalog_version: SEMANTIC_CATALOG_VERSION,
            request_id: new_request_id(),
            error: ErrorObject {
                code,
                required: true,
                field,
                parameters: serde_json::json!({}),
            },
        }),
    ))
}

fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
