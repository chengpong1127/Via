use std::time::Duration;

use axum::{
    body::Body,
    extract::MatchedPath,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use tower_http::request_id::{MakeRequestId, RequestId};
use tracing::{Span, field};

use crate::api::new_request_id;

use super::handlers::error_response;

#[derive(Clone, Copy, Default)]
pub(super) struct ViaMakeRequestId;

impl MakeRequestId for ViaMakeRequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        new_request_id().parse().ok().map(RequestId::new)
    }
}

pub(super) fn make_request_span(request: &Request<Body>) -> Span {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("missing_request_id");
    let method = request.method();
    let span_fields = || {
        (
            request_id,
            method,
            field::Empty,
            field::Empty,
            field::Empty,
            field::Empty,
            field::Empty,
        )
    };

    if request.uri().path() == "/v1/journey-plans" {
        let (request_id, method, status, outcome, journey_options, error_code, elapsed_ms) =
            span_fields();
        tracing::info_span!(
            "journey_plan",
            request_id = %request_id,
            method = %method,
            path = field::Empty,
            selected_modes = field::Empty,
            departure_setting = field::Empty,
            status,
            outcome,
            journey_options,
            error_code,
            elapsed_ms,
        )
    } else {
        let (request_id, method, status, outcome, journey_options, error_code, elapsed_ms) =
            span_fields();
        tracing::info_span!(
            "http_request",
            request_id = %request_id,
            method = %method,
            path = field::Empty,
            status,
            outcome,
            journey_options,
            error_code,
            elapsed_ms,
        )
    }
}

/// Records the matched route template as the span `path`, or "unmatched". The raw request path is
/// never logged: unrouted paths can contain user-supplied coordinates. Runs inside the router so
/// routing has already happened.
pub(super) async fn record_route_template(request: Request<Body>, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    Span::current().record("path", route);
    next.run(request).await
}

pub(super) fn on_http_response<B>(response: &Response<B>, latency: Duration, span: &Span) {
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

pub(super) async fn http_timeout(
    request: Request<Body>,
    next: Next,
    timeout: Duration,
) -> Response {
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(StatusCode::REQUEST_TIMEOUT, "request_timeout", None),
    }
}
