use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use utoipa::{
    openapi::schema::{ArrayItems, Object, Schema, Type},
    openapi::{InfoBuilder, OpenApi, RefOr},
};
use utoipa_axum::router::OpenApiRouter;

use crate::{
    api::{API_VERSION, PlanningRequest, SEMANTIC_CATALOG_VERSION, new_request_id},
    domain::transit::TransitNetwork,
    health::DependencyHealth,
    observability::OperationalMetrics,
    providers::realtime::{CachedRealtimeProvider, NullRealtimeProvider, RealtimeProvider},
    providers::timing::{
        CompositeDepartureResolver, MetroDepartureResolver, RealtimeDepartureResolver,
    },
    routing::{PlanningError, PlanningOutcome, RoutingService},
    street::{StreetRouter, StreetRoutingError},
};

#[derive(Clone)]
struct AppState {
    street_health: Arc<dyn DependencyHealth>,
    routing: RoutingService,
    transit_network: Arc<TransitNetwork>,
    metrics: OperationalMetrics,
    realtime_metrics: crate::providers::realtime::RealtimeMetrics,
    openapi: OpenApi,
}

pub async fn build_router<P>(provider: Arc<P>) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    build_router_with_realtime_and_network(
        provider,
        Arc::new(NullRealtimeProvider),
        TransitNetwork::default().with_revisions("development-static", "development-schedule"),
    )
    .await
}

pub async fn build_router_with_realtime<P>(
    provider: Arc<P>,
    realtime: Arc<dyn RealtimeProvider>,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    build_router_with_realtime_and_network(provider, realtime, TransitNetwork::default()).await
}

pub async fn build_router_with_network<P>(
    provider: Arc<P>,
    transit_network: TransitNetwork,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    build_router_with_realtime_and_network(
        provider,
        Arc::new(NullRealtimeProvider),
        transit_network,
    )
    .await
}

pub async fn build_router_with_realtime_and_network<P>(
    provider: Arc<P>,
    realtime: Arc<dyn RealtimeProvider>,
    transit_network: TransitNetwork,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    let (street_router, street_health) = provider_interfaces(provider);
    build_router_with_realtime_and_network_and_ttl_interfaces(
        street_router,
        street_health,
        realtime,
        transit_network,
        crate::providers::realtime::DEFAULT_REALTIME_CACHE_TTL,
    )
    .await
}

pub async fn build_router_with_realtime_and_network_and_ttl<P>(
    provider: Arc<P>,
    realtime: Arc<dyn RealtimeProvider>,
    transit_network: TransitNetwork,
    realtime_cache_ttl: std::time::Duration,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    let (street_router, street_health) = provider_interfaces(provider);
    build_router_with_realtime_and_network_and_ttl_interfaces(
        street_router,
        street_health,
        realtime,
        transit_network,
        realtime_cache_ttl,
    )
    .await
}

async fn build_router_with_realtime_and_network_and_ttl_interfaces(
    street_router: Arc<dyn StreetRouter>,
    street_health: Arc<dyn DependencyHealth>,
    realtime: Arc<dyn RealtimeProvider>,
    transit_network: TransitNetwork,
    realtime_cache_ttl: std::time::Duration,
) -> Result<Router, StreetRoutingError> {
    let realtime = Arc::new(CachedRealtimeProvider::with_ttl(
        realtime,
        realtime_cache_ttl,
    ));
    let realtime_metrics = realtime.metrics();
    let transit_network = Arc::new(transit_network);
    let metro_departure = Arc::new(MetroDepartureResolver::new(transit_network.clone()));
    let departure_resolver = Arc::new(
        CompositeDepartureResolver::with_schedule_fallback(
            transit_network.clone(),
            Arc::new(
                RealtimeDepartureResolver::new(realtime.clone(), transit_network.clone())
                    .with_metrics(realtime_metrics.clone()),
            ),
        )
        .with_adapter("metro_schedule", metro_departure.clone())
        .with_adapter("metro_frequency", metro_departure),
    );
    let metrics = OperationalMetrics::default();
    let routing = RoutingService::new(street_router, departure_resolver, realtime.clone())
        .with_transit_network_arc(transit_network.clone())
        .with_metrics(metrics.clone());
    let (router, openapi) = documented_router().split_for_parts();
    let openapi = configure_openapi(openapi);
    Ok(router
        .route("/openapi.json", get(openapi_document_endpoint))
        .with_state(AppState {
            street_health,
            routing,
            transit_network,
            metrics,
            realtime_metrics,
            openapi,
        })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(http_timeout)))
}

fn provider_interfaces<P>(provider: Arc<P>) -> (Arc<dyn StreetRouter>, Arc<dyn DependencyHealth>)
where
    P: StreetRouter + DependencyHealth + 'static,
{
    (provider.clone(), provider)
}

fn documented_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(utoipa_axum::routes!(liveness))
        .routes(utoipa_axum::routes!(readiness))
        .routes(utoipa_axum::routes!(metrics))
        .routes(utoipa_axum::routes!(street_routing_health))
        .routes(utoipa_axum::routes!(journey_plan))
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
    apply_schema_constraints(&mut document);
    document
}

// These invariants are enforced by Serde/runtime validation but cannot be
// expressed by utoipa's derive attributes for enum values and constants.
fn apply_schema_constraints(document: &mut OpenApi) {
    set_schema_enum(
        document,
        "JourneyMode",
        ["walking", "transit"].map(serde_json::Value::from),
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
        ["transit"].map(serde_json::Value::from),
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
        format!(
            "{}{}",
            state.metrics.prometheus(),
            state.realtime_metrics.prometheus()
        ),
    ))
}

#[derive(Serialize, utoipa::ToSchema)]
struct ReadinessResponse {
    ready: bool,
    street_routing: bool,
    static_network: bool,
    transit_artifact: bool,
}

#[utoipa::path(
    get,
    path = "/health/ready",
    operation_id = "getReadiness",
    summary = "Check planning readiness",
    description = "Reports whether street routing and the immutable static transit artifact are ready for planning.",
    responses(
        (status = 200, description = "All planning dependencies are ready", body = ReadinessResponse,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 503, description = "One or more planning dependencies are unavailable", body = ReadinessResponse,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
async fn readiness(State(state): State<AppState>) -> Response {
    let street_routing = state.street_health.healthy().await;
    let static_network = !state
        .transit_network
        .static_network_revision
        .trim()
        .is_empty()
        && !state.transit_network.schedule_revision.trim().is_empty();
    let transit_artifact = state.transit_network.validate().is_ok();
    let body = ReadinessResponse {
        ready: street_routing && static_network && transit_artifact,
        street_routing,
        static_network,
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
    provider: String,
}

#[utoipa::path(
    get,
    path = "/health/street-routing",
    operation_id = "getStreetRoutingHealth",
    summary = "Check street-routing health",
    description = "Reports whether the street-routing dependency is healthy.",
    responses(
        (status = 200, description = "Street-routing provider is healthy", body = HealthResponse,
            headers(("Cache-Control" = String, description = "Always no-store."))),
        (status = 503, description = "Street-routing provider is unavailable", body = HealthResponse,
            headers(("Cache-Control" = String, description = "Always no-store.")))
    )
)]
async fn street_routing_health(State(state): State<AppState>) -> Response {
    let healthy = state.street_health.healthy().await;
    let body = HealthResponse {
        healthy,
        provider: state.street_health.name().to_owned(),
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

#[utoipa::path(
    post,
    path = "/v1/journey-plans",
    operation_id = "createJourneyPlan",
    summary = "Plan a journey",
    description = "Calculates provider-neutral Journey Options from one immutable static network revision, with demand-driven realtime resolution.",
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
    match state.routing.route(request).await {
        Ok(outcome) => {
            match &outcome {
                PlanningOutcome::Options(result) => state
                    .metrics
                    .record_planning_success(result.journey_options.len()),
                PlanningOutcome::Empty(_) => state.metrics.record_planning_success(0),
            }
            no_store((StatusCode::OK, Json(outcome)))
        }
        Err(PlanningError::TemporarilyUnavailable) => {
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
