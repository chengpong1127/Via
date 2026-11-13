use std::{sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};

use super::{LiveBatch, LiveDataSource, MetroPair};

pub const BUS_ARRIVALS_PATH: &str = "live/bus/arrivals";
pub const BIKE_AVAILABILITY_PATH: &str = "live/bike/availability";
pub const METRO_BOARDS_PATH: &str = "live/metro/boards";
pub const TRA_DELAYS_PATH: &str = "live/tra/delays";

const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRequest<K = String> {
    #[serde(default)]
    pub scope: String,
    pub ids: Vec<K>,
}

#[derive(Clone)]
struct GatewayState {
    live: Arc<dyn LiveDataSource>,
    metrics: PrometheusHandle,
    max_ids_per_request: usize,
}

pub fn router(
    live: Arc<dyn LiveDataSource>,
    metrics: PrometheusHandle,
    max_ids_per_request: usize,
) -> Router {
    Router::new()
        .route(&format!("/{BUS_ARRIVALS_PATH}"), post(bus_arrivals))
        .route(
            &format!("/{BIKE_AVAILABILITY_PATH}"),
            post(bike_availability),
        )
        .route(&format!("/{METRO_BOARDS_PATH}"), post(metro_boards))
        .route(&format!("/{TRA_DELAYS_PATH}"), post(tra_delays))
        .route("/health/live", get(health))
        .route("/health/ready", get(health))
        .route("/metrics", get(metrics_endpoint))
        .with_state(GatewayState {
            live,
            metrics,
            max_ids_per_request,
        })
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
}

async fn bus_arrivals(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest>,
) -> Response {
    if let Some(rejection) = state.reject("bus", &request.ids) {
        return rejection;
    }
    let started = Instant::now();
    let batch = state.live.bus_arrivals(&request.scope, &request.ids).await;
    respond("bus", &request.scope, request.ids.len(), started, batch)
}

async fn bike_availability(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest>,
) -> Response {
    if let Some(rejection) = state.reject("bike", &request.ids) {
        return rejection;
    }
    let started = Instant::now();
    let batch = state
        .live
        .bike_availability(&request.scope, &request.ids)
        .await;
    respond("bike", &request.scope, request.ids.len(), started, batch)
}

async fn metro_boards(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest<MetroPair>>,
) -> Response {
    if let Some(rejection) = state.reject("metro", &request.ids) {
        return rejection;
    }
    let started = Instant::now();
    let batch = state.live.metro_boards(&request.ids).await;
    respond("metro", &request.scope, request.ids.len(), started, batch)
}

async fn tra_delays(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest>,
) -> Response {
    if let Some(rejection) = state.reject("tra", &request.ids) {
        return rejection;
    }
    let started = Instant::now();
    let batch = state.live.tra_delays(&request.ids).await;
    respond("tra", &request.scope, request.ids.len(), started, batch)
}

fn respond<T: Serialize, K: Clone + Serialize>(
    dataset: &'static str,
    scope: &str,
    requested: usize,
    started: Instant,
    batch: LiveBatch<T, K>,
) -> Response {
    let elapsed = started.elapsed();
    ::metrics::counter!("via_tdx_gateway_requests_total", "dataset" => dataset).increment(1);
    ::metrics::histogram!("via_tdx_gateway_request_seconds", "dataset" => dataset)
        .record(elapsed.as_secs_f64());
    tracing::info!(
        target: "via_server::live::gateway",
        dataset,
        scope,
        requested,
        records = batch.records.len(),
        unavailable = batch.unavailable.len(),
        rate_limited = batch.is_rate_limited(),
        upstream_requests = batch.upstream_requests,
        elapsed_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
        "live data request"
    );
    Json(batch).into_response()
}

async fn health() -> Response {
    (StatusCode::OK, [("cache-control", "no-store")], "ok").into_response()
}

async fn metrics_endpoint(State(state): State<GatewayState>) -> Response {
    (
        StatusCode::OK,
        [
            ("content-type", "text/plain; version=0.0.4; charset=utf-8"),
            ("cache-control", "no-store"),
        ],
        state.metrics.render(),
    )
        .into_response()
}

impl GatewayState {
    fn reject<K>(&self, dataset: &'static str, ids: &[K]) -> Option<Response> {
        (ids.len() > self.max_ids_per_request).then(|| {
            ::metrics::counter!("via_tdx_gateway_rejected_requests_total", "dataset" => dataset)
                .increment(1);
            tracing::warn!(
                target: "via_server::live::gateway",
                dataset,
                requested = ids.len(),
                limit = self.max_ids_per_request,
                "live data request rejected for too many ids"
            );
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "at most {} ids are accepted per request",
                    self.max_ids_per_request
                ),
            )
                .into_response()
        })
    }
}
