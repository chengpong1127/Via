use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Extension, State, rejection::JsonRejection},
    http::{HeaderValue, Request, StatusCode, header::CACHE_CONTROL},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use tower_http::{
    request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::{Span, field};
use utoipa::{
    openapi::schema::{ArrayItems, Object, Schema, Type},
    openapi::{InfoBuilder, OpenApi, RefOr},
};
use utoipa_axum::router::OpenApiRouter;

use crate::{
    api::{API_VERSION, PlanningRequest, SEMANTIC_CATALOG_VERSION, new_request_id},
    domain::{
        bikeshare::BikeShareNetwork,
        journey::{PlanningError, PlanningOutcome},
        transit::TransitNetwork,
        transit_runtime::TransitRuntime,
    },
    health::DependencyHealth,
    observability::OperationalMetrics,
    providers::bikeshare::BikeAvailabilitySource,
    providers::bindings::TdxProviderBindings,
    providers::service_conditions::{
        CachedServiceConditionProvider, DEFAULT_SERVICE_CONDITION_CACHE_TTL,
        ServiceConditionMetrics, ServiceConditionProvider,
    },
    providers::timing::{
        BusDepartureSource, BusTimingMetrics, DepartureDispatcher, MetroDepartureSource,
        ThsrDepartureSource, TraDepartureSource, TransitDepartureSource,
    },
    routing::{Phase2Strategy, RoutingLimits, RoutingService},
    street::{StreetRouter, StreetRoutingError},
    tdx::TdxClient,
};

#[derive(Clone)]
struct AppState {
    street_health: Arc<dyn DependencyHealth>,
    routing: RoutingService,
    transit_runtime: Arc<TransitRuntime>,
    metrics: OperationalMetrics,
    service_condition_metrics: ServiceConditionMetrics,
    bus_timing_metrics: BusTimingMetrics,
    openapi: OpenApi,
}

#[derive(Clone)]
pub struct RouterDependencies {
    service_conditions: Arc<dyn ServiceConditionProvider>,
    tdx: Option<Arc<TdxClient>>,
    service_condition_cache_ttl: Duration,
    bike_share_network: Arc<BikeShareNetwork>,
    bike_availability_source: Arc<dyn BikeAvailabilitySource>,
    provider_bindings: Arc<TdxProviderBindings>,
    phase2_strategy: Phase2Strategy,
}

impl RouterDependencies {
    pub fn new(
        service_conditions: Arc<dyn ServiceConditionProvider>,
        bike_share_network: Arc<BikeShareNetwork>,
        bike_availability_source: Arc<dyn BikeAvailabilitySource>,
    ) -> Self {
        Self {
            service_conditions,
            tdx: None,
            service_condition_cache_ttl: DEFAULT_SERVICE_CONDITION_CACHE_TTL,
            bike_share_network,
            bike_availability_source,
            provider_bindings: Arc::new(TdxProviderBindings::default()),
            phase2_strategy: Phase2Strategy::Union,
        }
    }

    pub fn with_tdx(mut self, tdx: Arc<TdxClient>) -> Self {
        self.tdx = Some(tdx);
        self
    }

    pub fn with_provider_bindings(mut self, bindings: TdxProviderBindings) -> Self {
        self.provider_bindings = Arc::new(bindings);
        self
    }

    pub fn with_service_condition_cache_ttl(mut self, ttl: Duration) -> Self {
        self.service_condition_cache_ttl = ttl;
        self
    }

    pub fn with_phase2_strategy(mut self, strategy: Phase2Strategy) -> Self {
        self.phase2_strategy = strategy;
        self
    }
}

pub async fn build_router<P>(
    provider: Arc<P>,
    dependencies: RouterDependencies,
    transit_network: TransitNetwork,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    let (street_router, street_health) = provider_interfaces(provider);
    let service_conditions = Arc::new(CachedServiceConditionProvider::with_ttl(
        dependencies.service_conditions,
        dependencies.service_condition_cache_ttl,
    ));
    let service_condition_metrics = service_conditions.metrics();
    let bus_timing_metrics = BusTimingMetrics::default();
    let transit_runtime = Arc::new(TransitRuntime::new(Arc::new(transit_network)));
    let bus = Arc::new(
        BusDepartureSource::new(transit_runtime.clone(), dependencies.tdx.clone())
            .with_bindings(dependencies.provider_bindings.clone())
            .with_metrics(bus_timing_metrics.clone()),
    );
    let metro = Arc::new(
        MetroDepartureSource::new(transit_runtime.clone())
            .with_bindings(dependencies.provider_bindings.clone())
            .with_tdx(dependencies.tdx.clone()),
    );
    let tra = Arc::new(
        TraDepartureSource::new(transit_runtime.clone())
            .with_bindings(dependencies.provider_bindings)
            .with_tdx(dependencies.tdx),
    );
    let thsr = Arc::new(ThsrDepartureSource::new(transit_runtime.clone()));
    let departure_resolver = Arc::new(DepartureDispatcher::new(
        transit_runtime.clone(),
        vec![
            bus as Arc<dyn TransitDepartureSource>,
            metro as Arc<dyn TransitDepartureSource>,
            tra as Arc<dyn TransitDepartureSource>,
            thsr as Arc<dyn TransitDepartureSource>,
        ],
    ));
    let phase2_strategy = dependencies.phase2_strategy;
    let routing = RoutingService::new(
        street_router,
        transit_runtime.clone(),
        departure_resolver,
        service_conditions,
        dependencies.bike_share_network,
        dependencies.bike_availability_source,
        RoutingLimits::default(),
    )
    .with_phase2_strategy(phase2_strategy);
    let metrics = routing.operational_metrics.clone();
    let (router, openapi) = documented_router().split_for_parts();
    let openapi = configure_openapi(openapi);
    Ok(router
        .route("/openapi.json", get(openapi_document_endpoint))
        .with_state(AppState {
            street_health,
            routing,
            transit_runtime,
            metrics,
            service_condition_metrics,
            bus_timing_metrics,
            openapi,
        })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(http_timeout))
        // These layers are listed in response-path order. SetRequestId must run
        // before TraceLayer on the request path, while propagation must run
        // before TraceLayer observes the response.
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_request_span)
                .on_response(on_http_response)
                .on_failure(()),
        )
        .layer(SetRequestIdLayer::x_request_id(ViaMakeRequestId)))
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
        ["walking", "transit", "bicycle"].map(serde_json::Value::from),
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
        ["bus", "metro", "tra", "thsr", "bike_share"].map(serde_json::Value::from),
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

#[derive(Clone, Copy, Default)]
struct ViaMakeRequestId;

impl MakeRequestId for ViaMakeRequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        new_request_id().parse().ok().map(RequestId::new)
    }
}

fn request_id_string(request_id: &RequestId) -> String {
    request_id
        .header_value()
        .to_str()
        .unwrap_or("invalid_request_id")
        .to_owned()
}

fn make_request_span(request: &Request<Body>) -> Span {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("missing_request_id");
    let method = request.method();
    let path = request.uri().path();
    let span_fields = || {
        (
            request_id,
            method,
            path,
            field::Empty,
            field::Empty,
            field::Empty,
            field::Empty,
            field::Empty,
        )
    };

    if path == "/v1/journey-plans" {
        let (request_id, method, path, status, outcome, journey_options, error_code, elapsed_ms) =
            span_fields();
        tracing::info_span!(
            "journey_plan",
            request_id = %request_id,
            method = %method,
            path = %path,
            selected_modes = field::Empty,
            departure_setting = field::Empty,
            status,
            outcome,
            journey_options,
            error_code,
            elapsed_ms,
        )
    } else {
        let (request_id, method, path, status, outcome, journey_options, error_code, elapsed_ms) =
            span_fields();
        tracing::info_span!(
            "http_request",
            request_id = %request_id,
            method = %method,
            path = %path,
            status,
            outcome,
            journey_options,
            error_code,
            elapsed_ms,
        )
    }
}

fn on_http_response<B>(response: &Response<B>, latency: Duration, span: &Span) {
    let status = u64::from(response.status().as_u16());
    let elapsed_ms = latency.as_millis().min(u128::from(u64::MAX)) as u64;
    span.record("status", status);
    span.record("elapsed_ms", elapsed_ms);
    tracing::info!(
        target: "via_server::http",
        status,
        elapsed_ms,
        "request completed"
    );
}

async fn http_timeout(request: Request<Body>, next: Next) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(request_id_string)
        .unwrap_or_else(|| "missing_request_id".into());
    match tokio::time::timeout(std::time::Duration::from_secs(10), next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(
            &request_id,
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            None,
        ),
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
            "{}{}{}",
            state.metrics.prometheus(),
            state.service_condition_metrics.prometheus(),
            state.bus_timing_metrics.prometheus()
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
    // Readiness remains deliberately independent of realtime availability.
    let street_routing = state.street_health.healthy().await;
    let revisions = state.transit_runtime.data().revisions();
    let static_network = !revisions.0.trim().is_empty() && !revisions.1.trim().is_empty();
    let transit_artifact = state.transit_runtime.data().validate().is_ok();
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
    Extension(request_id): Extension<RequestId>,
    request: Result<Json<PlanningRequest>, JsonRejection>,
) -> Response {
    let request_id = request_id_string(&request_id);
    let request = match request {
        Ok(Json(request)) => request,
        Err(_) => {
            return error_response(
                &request_id,
                StatusCode::BAD_REQUEST,
                "invalid_request",
                Some("/"),
            );
        }
    };
    if let Err(error) = request.validate() {
        return error_response(
            &request_id,
            StatusCode::BAD_REQUEST,
            error.code(),
            Some(error.field()),
        );
    }
    record_request_fields(&request);
    match state.routing.route(request, request_id.clone()).await {
        Ok(outcome) => {
            let journey_options = match &outcome {
                PlanningOutcome::Options(result) => result.journey_options.len(),
                PlanningOutcome::Empty(_) => 0,
            };
            state.metrics.record_planning_success(journey_options);
            Span::current().record(
                "outcome",
                if journey_options == 0 {
                    "empty"
                } else {
                    "options"
                },
            );
            Span::current().record("journey_options", journey_options as u64);
            no_store((StatusCode::OK, Json(outcome)))
        }
        Err(PlanningError::TemporarilyUnavailable) => {
            state.metrics.record_planning_failure();
            error_response(
                &request_id,
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                None,
            )
        }
        Err(PlanningError::Internal) => {
            state.metrics.record_planning_failure();
            error_response(
                &request_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "planner_failure",
                None,
            )
        }
    }
}

fn record_request_fields(request: &PlanningRequest) {
    let span = Span::current();
    span.record(
        "selected_modes",
        field::debug(request.canonical_selected_modes()),
    );
    span.record(
        "departure_setting",
        match request.departure_setting {
            crate::api::DepartureSetting::DepartNow => "depart_now",
            crate::api::DepartureSetting::DepartAt { .. } => "depart_at",
            crate::api::DepartureSetting::ArriveBy { .. } => "arrive_by",
        },
    );
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

fn error_response(
    request_id: &str,
    status: StatusCode,
    code: &str,
    field: Option<&str>,
) -> Response {
    Span::current().record("outcome", "error");
    Span::current().record("error_code", code);
    no_store((
        status,
        Json(ErrorEnvelope {
            api_version: API_VERSION,
            semantic_catalog_version: SEMANTIC_CATALOG_VERSION,
            request_id: request_id.to_owned(),
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
