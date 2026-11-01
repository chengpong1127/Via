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

use crate::{config::Environment, health::Health, nearby::NearbyService, routing::RoutingService};

#[derive(Clone)]
struct AppState {
    routing: RoutingService,
    health: Health,
    nearby: NearbyService,
    metrics: PrometheusHandle,
    openapi: OpenApi,
}

pub fn build_router(
    routing: RoutingService,
    health: Health,
    nearby: NearbyService,
    metrics: PrometheusHandle,
    environment: Environment,
    settings: &crate::config::ServerSettings,
) -> Router {
    let (router, openapi) = openapi::documented_router().split_for_parts();
    let openapi = openapi::configure_openapi(openapi);
    let router = router
        .route(
            "/openapi.json",
            axum::routing::get(handlers::openapi_document_endpoint),
        )
        .with_state(AppState {
            routing,
            health,
            nearby,
            metrics,
            openapi,
        })
        .layer(axum::extract::DefaultBodyLimit::max(
            settings.max_body_bytes,
        ));
    with_http_timeout(
        router,
        environment,
        std::time::Duration::from_millis(settings.http_timeout_ms),
    )
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

fn with_http_timeout(
    router: Router,
    environment: Environment,
    timeout: std::time::Duration,
) -> Router {
    if environment.is_production() {
        router.layer(axum::middleware::from_fn(move |request, next| {
            middleware::http_timeout(request, next, timeout)
        }))
    } else {
        router
    }
}
