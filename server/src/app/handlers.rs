use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tracing::{Span, field};

use crate::{
    api::PlanningRequest,
    domain::journey::{PlanningError, PlanningOutcome},
};

use super::AppState;

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
pub(super) async fn liveness() -> Response {
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
pub(super) async fn metrics(State(state): State<AppState>) -> Response {
    no_store((
        StatusCode::OK,
        [
            ("content-type", "text/plain; version=0.0.4"),
            ("cache-control", "no-store"),
        ],
        format!(
            "{}{}",
            state.metrics.prometheus(),
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
pub(super) async fn readiness(State(state): State<AppState>) -> Response {
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

pub(super) async fn openapi_document_endpoint(State(state): State<AppState>) -> Response {
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
pub(super) async fn street_routing_health(State(state): State<AppState>) -> Response {
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
pub(super) async fn journey_plan(
    State(state): State<AppState>,
    request: Result<Json<PlanningRequest>, JsonRejection>,
) -> Response {
    let request = match request {
        Ok(Json(request)) => request,
        Err(_) => {
            return error_response(StatusCode::BAD_REQUEST, "invalid_request", Some("/"));
        }
    };
    if let Err(error) = request.validate() {
        return error_response(StatusCode::BAD_REQUEST, error.code(), Some(error.field()));
    }
    record_request_fields(&request);
    match state.routing.route(request).await {
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
    error: ErrorObject,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ErrorObject {
    code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<String>,
}

pub(super) fn error_response(status: StatusCode, code: &str, field: Option<&str>) -> Response {
    Span::current().record("outcome", "error");
    Span::current().record("error_code", code);
    no_store((
        status,
        Json(ErrorEnvelope {
            error: ErrorObject {
                code: code.into(),
                field: field.map(str::to_owned),
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
