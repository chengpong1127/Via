mod handlers;
mod middleware;
mod openapi;

use axum::Router;
use metrics_exporter_prometheus::PrometheusHandle;
use tower_http::{
    request_id::{PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use utoipa::openapi::OpenApi;

use crate::{health::Health, routing::RoutingService};

#[derive(Clone)]
struct AppState {
    routing: RoutingService,
    health: Health,
    metrics: PrometheusHandle,
    openapi: OpenApi,
}

pub fn build_router(routing: RoutingService, health: Health, metrics: PrometheusHandle) -> Router {
    let (router, openapi) = openapi::documented_router().split_for_parts();
    let openapi = openapi::configure_openapi(openapi);
    router
        .route(
            "/openapi.json",
            axum::routing::get(handlers::openapi_document_endpoint),
        )
        .with_state(AppState {
            routing,
            health,
            metrics,
            openapi,
        })
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(axum::middleware::from_fn(middleware::http_timeout))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(middleware::make_request_span)
                .on_response(middleware::on_http_response)
                .on_failure(()),
        )
        .layer(SetRequestIdLayer::x_request_id(
            middleware::ViaMakeRequestId,
        ))
}

pub fn openapi_document() -> OpenApi {
    openapi::openapi_document()
}
