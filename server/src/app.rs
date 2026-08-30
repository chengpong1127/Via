use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use crate::street::{
    GraphRevision, ProviderCapabilities, ProviderCategory, ProviderError, StreetRoutingProvider,
    StreetRoutingSession,
};

#[derive(Clone)]
struct AppState {
    street_routing: StreetRoutingSession,
}

pub async fn build_router(
    provider: Arc<dyn StreetRoutingProvider>,
) -> Result<Router, ProviderError> {
    let street_routing = StreetRoutingSession::start(provider).await?;
    Ok(Router::new()
        .route("/health/street-routing", get(street_routing_health))
        .with_state(AppState { street_routing }))
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

fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
