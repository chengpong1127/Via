use std::time::Duration;

use axum::{
    body::Body,
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

pub(super) fn request_id_string(request_id: &RequestId) -> String {
    request_id
        .header_value()
        .to_str()
        .unwrap_or("invalid_request_id")
        .to_owned()
}

pub(super) fn make_request_span(request: &Request<Body>) -> Span {
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

pub(super) async fn http_timeout(request: Request<Body>, next: Next) -> Response {
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
