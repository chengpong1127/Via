use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use time::{OffsetDateTime, UtcOffset};
use utoipa::{
    openapi::schema::{ArrayItems, Object, Schema, Type},
    openapi::{InfoBuilder, OpenApi, RefOr},
};
use utoipa_axum::router::OpenApiRouter;

use crate::{
    api::{API_VERSION, PlanningRequest, SEMANTIC_CATALOG_VERSION, new_request_id, taipei_offset},
    observability::OperationalMetrics,
    planner::{JourneyPlanningService, PlanningError, PlanningOutcome},
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
    openapi: OpenApi,
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
    let (router, openapi) = documented_router().split_for_parts();
    let openapi = configure_openapi(openapi);
    Ok(router
        .route("/openapi.json", get(openapi_document_endpoint))
        .with_state(AppState {
            street_routing,
            journey_planning,
            refresh: refresh_endpoint,
            snapshots,
            transit_network: Arc::new(transit_network),
            metrics: OperationalMetrics::default(),
            openapi,
        })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(http_timeout)))
}

fn documented_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(utoipa_axum::routes!(liveness))
        .routes(utoipa_axum::routes!(readiness))
        .routes(utoipa_axum::routes!(metrics))
        .routes(utoipa_axum::routes!(street_routing_health))
        .routes(utoipa_axum::routes!(journey_plan))
        .routes(utoipa_axum::routes!(refresh))
}

pub fn openapi_document() -> OpenApi {
    configure_openapi(documented_router().into_openapi())
}

fn configure_openapi(mut document: OpenApi) -> OpenApi {
    document.info = InfoBuilder::new()
        .title("Via API")
        .version(API_VERSION)
        .description(Some(
            "Provider-neutral Journey Planning and service health API.",
        ))
        .build();
    let components = document
        .components
        .get_or_insert_with(utoipa::openapi::Components::new);
    components.add_security_scheme(
        "refreshBearer",
        utoipa::openapi::security::SecurityScheme::Http(
            utoipa::openapi::security::HttpBuilder::new()
                .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                .description(Some("Bearer token for the internal refresh scheduler."))
                .build(),
        ),
    );
    apply_schema_constraints(&mut document);
    document
}

// These invariants are enforced by Serde/runtime validation but cannot be
// expressed by utoipa's derive attributes for enum values and constants.
fn apply_schema_constraints(document: &mut OpenApi) {
    set_schema_enum(
        document,
        "JourneyMode",
        ["walking", "bike_share", "transit"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "LivenessResponse",
        "alive",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ResultMetadata",
        "api_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ResultMetadata",
        "semantic_catalog_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ResultMetadata",
        "time_zone",
        ["Asia/Taipei"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "Warning",
        "required",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "Warning",
        "severity",
        ["information", "caution"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "SemanticObject",
        "required",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ErrorEnvelope",
        "api_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ErrorEnvelope",
        "semantic_catalog_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ErrorObject",
        "required",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "Geometry",
        "encoding",
        ["polyline6"].map(serde_json::Value::from),
    );
    set_schema_property_unique_items(document, "PlanningRequest", "selected_modes");
    set_schema_property_items_enum(
        document,
        "PlanningRequest",
        "selected_modes",
        ["bike_share", "transit"].map(serde_json::Value::from),
    );
}

fn set_schema_enum<I>(document: &mut OpenApi, schema_name: &str, values: I)
where
    I: IntoIterator<Item = serde_json::Value>,
{
    if let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
    {
        schema.enum_values = Some(values.into_iter().collect());
    }
}

fn set_schema_property_enum<I>(
    document: &mut OpenApi,
    schema_name: &str,
    property_name: &str,
    values: I,
) where
    I: IntoIterator<Item = serde_json::Value>,
{
    if let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
        && let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(property))) =
            schema.properties.get_mut(property_name)
    {
        property.enum_values = Some(values.into_iter().collect());
    }
}

fn set_schema_property_unique_items(
    document: &mut OpenApi,
    schema_name: &str,
    property_name: &str,
) {
    if let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
        && let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Array(property))) =
            schema.properties.get_mut(property_name)
    {
        property.unique_items = true;
    }
}

fn set_schema_property_items_enum<I>(
    document: &mut OpenApi,
    schema_name: &str,
    property_name: &str,
    values: I,
) where
    I: IntoIterator<Item = serde_json::Value>,
{
    if let Some(RefOr::T(Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
        && let Some(RefOr::T(Schema::Array(property))) = schema.properties.get_mut(property_name)
    {
        let mut item_schema = Object::with_type(Type::String);
        item_schema.enum_values = Some(values.into_iter().collect());
        property.items = ArrayItems::RefOrSchema(Box::new(RefOr::T(Schema::Object(item_schema))));
    }
}

async fn http_timeout(request: axum::http::Request<Body>, next: Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(10), next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(StatusCode::REQUEST_TIMEOUT, "request_timeout", None),
    }
}

#[derive(Serialize, utoipa::ToSchema)]
struct LivenessResponse {
    alive: bool,
}

#[utoipa::path(
    get,
    path = "/health/live",
    operation_id = "getLiveness",
    summary = "Check process liveness",
    description = "Returns whether the server process is alive.",
    responses(
        (status = 200, description = "The process is alive", body = LivenessResponse,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
async fn liveness() -> Response {
    no_store((StatusCode::OK, Json(LivenessResponse { alive: true })))
}

#[utoipa::path(
    get,
    path = "/metrics",
    operation_id = "getOperationalMetrics",
    summary = "Read operational metrics",
    description = "Returns location-free Prometheus metrics for service operation.",
    responses(
        (status = 200, description = "Location-free operational metrics", content_type = "text/plain", body = String,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
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

#[derive(Serialize, utoipa::ToSchema)]
struct ReadinessResponse {
    ready: bool,
    street_routing: bool,
    network_snapshot: bool,
    transit_artifact: bool,
}

#[utoipa::path(
    get,
    path = "/health/ready",
    operation_id = "getReadiness",
    summary = "Check planning readiness",
    description = "Reports whether the street-routing provider, Network Snapshot, and transit artifact are ready for planning.",
    responses(
        (status = 200, description = "All planning dependencies are ready", body = ReadinessResponse,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 503, description = "One or more planning dependencies are unavailable", body = ReadinessResponse,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
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

async fn openapi_document_endpoint(State(state): State<AppState>) -> Response {
    no_store((StatusCode::OK, Json(state.openapi.clone())))
}

#[derive(Serialize, utoipa::ToSchema)]
struct HealthResponse {
    healthy: bool,
    provider: ProviderCategory,
    graph_revision: GraphRevision,
    pinned_graph_revision: GraphRevision,
    capabilities: ProviderCapabilities,
}

#[utoipa::path(
    get,
    path = "/health/street-routing",
    operation_id = "getStreetRoutingHealth",
    summary = "Check street-routing health",
    description = "Reports the current provider health against the graph revision pinned at server startup.",
    responses(
        (status = 200, description = "Street-routing provider is healthy", body = HealthResponse,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 503, description = "Street-routing provider is unavailable or changed revision", body = HealthResponse,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
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

#[utoipa::path(
    post,
    path = "/v1/journey-plans",
    operation_id = "createJourneyPlan",
    summary = "Plan a journey",
    description = "Calculates provider-neutral Journey Options from one coherent Network Snapshot.",
    request_body(content = PlanningRequest, description = "Planning Request to evaluate.", content_type = "application/json"),
    responses(
        (status = 200, description = "A complete Journey Planning result or a successful empty result", body = PlanningOutcome,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 400, description = "The request is invalid", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 408, description = "The planning request timed out", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 500, description = "Planning failed unexpectedly", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 503, description = "Planning dependencies are temporarily unavailable", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
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

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct RefreshRequest {
    #[schema(min_length = 1, max_length = 128, pattern = "^[A-Za-z0-9_.:-]+$")]
    refresh_key: String,
}

#[utoipa::path(
    post,
    path = "/internal/v1/refresh",
    operation_id = "refreshNetworkSnapshot",
    summary = "Refresh the Network Snapshot",
    description = "Publishes one idempotent, coherent Network Snapshot from the configured feed sources.",
    request_body(content = RefreshRequest, description = "Scheduler refresh key.", content_type = "application/json"),
    security(("refreshBearer" = [])),
    responses(
        (status = 200, description = "The refresh was published or had already been handled", body = RefreshOutcome,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 400, description = "The refresh key is invalid", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 401, description = "The scheduler Bearer token is missing or invalid", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 500, description = "Refresh publication failed", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 503, description = "The refresh endpoint is not configured", body = ErrorEnvelope,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
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

#[derive(Serialize, utoipa::ToSchema)]
struct ErrorEnvelope {
    api_version: &'static str,
    semantic_catalog_version: &'static str,
    request_id: String,
    error: ErrorObject,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ErrorObject {
    code: String,
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<String>,
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
                code: code.into(),
                required: true,
                field: field.map(str::to_owned),
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
