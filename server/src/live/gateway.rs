use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;

use super::{LiveDataSource, MetroPair};

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
        .layer(TraceLayer::new_for_http())
}

async fn bus_arrivals(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest>,
) -> Response {
    if let Some(rejection) = state.reject(&request.ids) {
        return rejection;
    }
    Json(state.live.bus_arrivals(&request.scope, &request.ids).await).into_response()
}

async fn bike_availability(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest>,
) -> Response {
    if let Some(rejection) = state.reject(&request.ids) {
        return rejection;
    }
    Json(
        state
            .live
            .bike_availability(&request.scope, &request.ids)
            .await,
    )
    .into_response()
}

async fn metro_boards(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest<MetroPair>>,
) -> Response {
    if let Some(rejection) = state.reject(&request.ids) {
        return rejection;
    }
    Json(state.live.metro_boards(&request.ids).await).into_response()
}

async fn tra_delays(
    State(state): State<GatewayState>,
    Json(request): Json<LiveRequest>,
) -> Response {
    if let Some(rejection) = state.reject(&request.ids) {
        return rejection;
    }
    Json(state.live.tra_delays(&request.ids).await).into_response()
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
    fn reject<K>(&self, ids: &[K]) -> Option<Response> {
        (ids.len() > self.max_ids_per_request).then(|| {
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
