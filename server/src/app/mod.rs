mod handlers;
pub mod health;
pub mod metrics;
mod middleware;
pub mod nearby;
mod openapi;
pub mod profiling;
pub mod runtime;

use std::{sync::Arc, time::Duration};

use axum::Router;
use metrics_exporter_prometheus::PrometheusHandle;
use tower_http::{
    request_id::{PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use utoipa::openapi::OpenApi;

use crate::routing::RoutingService;
use health::Health;
use nearby::NearbyService;
use runtime::ViaRuntime;

#[derive(Clone)]
struct AppState {
    routing: RoutingService,
    health: Health,
    nearby: NearbyService,
    metrics: PrometheusHandle,
    openapi: Arc<OpenApi>,
}

pub fn build_router(
    runtime: ViaRuntime,
    metrics: PrometheusHandle,
    http_timeout: Option<Duration>,
    max_body_bytes: usize,
) -> Router {
    let (router, openapi) = openapi::documented_router().split_for_parts();
    let openapi = Arc::new(openapi::configure_openapi(openapi));
    let router = router
        .route(
            "/openapi.json",
            axum::routing::get(handlers::openapi_document_endpoint),
        )
        .with_state(AppState {
            routing: runtime.routing,
            health: runtime.health,
            nearby: runtime.nearby,
            metrics,
            openapi,
        })
        .layer(axum::extract::DefaultBodyLimit::max(max_body_bytes));
    with_http_timeout(router, http_timeout)
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

fn with_http_timeout(router: Router, timeout: Option<Duration>) -> Router {
    match timeout {
        Some(timeout) => router.layer(axum::middleware::from_fn(move |request, next| {
            middleware::http_timeout(request, next, timeout)
        })),
        None => router,
    }
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.expect("wait for Ctrl-C signal");
            }
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("wait for Ctrl-C signal");
}
