use serde::Serialize;

use crate::street::StreetRoutingError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Success,
    InvalidRequest,
    Unreachable,
    Timeout,
    RateLimited,
    Unavailable,
    MalformedResponse,
    ResponseTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingOperation {
    Health,
    Matrix,
    Route,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OperationalEvent {
    pub operation: RoutingOperation,
    pub provider: &'static str,
    pub latency_milliseconds: u64,
    pub outcome: OperationOutcome,
    pub source_count: usize,
    pub target_count: usize,
}

pub trait OperationalEventObserver: Send + Sync {
    fn observe(&self, event: OperationalEvent);
}

pub struct NoopOperationalEventObserver;

impl OperationalEventObserver for NoopOperationalEventObserver {
    fn observe(&self, _event: OperationalEvent) {}
}

#[derive(Default)]
pub struct TracingOperationalEventObserver;

impl OperationalEventObserver for TracingOperationalEventObserver {
    fn observe(&self, event: OperationalEvent) {
        let operation = operation_name(event.operation);
        let outcome = outcome_name(event.outcome);
        match event.outcome {
            OperationOutcome::Success => tracing::debug!(
                target: "via_server::providers::valhalla",
                provider = event.provider,
                operation,
                outcome,
                latency_ms = event.latency_milliseconds,
                sources = event.source_count,
                targets = event.target_count,
                "street routing provider operation completed"
            ),
            _ => tracing::warn!(
                target: "via_server::providers::valhalla",
                provider = event.provider,
                operation,
                outcome,
                latency_ms = event.latency_milliseconds,
                sources = event.source_count,
                targets = event.target_count,
                "street routing provider operation failed"
            ),
        }
    }
}

fn operation_name(operation: RoutingOperation) -> &'static str {
    match operation {
        RoutingOperation::Health => "health",
        RoutingOperation::Matrix => "matrix",
        RoutingOperation::Route => "route",
    }
}

fn outcome_name(outcome: OperationOutcome) -> &'static str {
    match outcome {
        OperationOutcome::Success => "success",
        OperationOutcome::InvalidRequest => "invalid_request",
        OperationOutcome::Unreachable => "unreachable",
        OperationOutcome::Timeout => "timeout",
        OperationOutcome::RateLimited => "rate_limited",
        OperationOutcome::Unavailable => "unavailable",
        OperationOutcome::MalformedResponse => "malformed_response",
        OperationOutcome::ResponseTooLarge => "response_too_large",
    }
}

pub(super) fn outcome_for(error: &StreetRoutingError) -> OperationOutcome {
    match error {
        StreetRoutingError::InvalidRequest { .. } => OperationOutcome::InvalidRequest,
        StreetRoutingError::Unreachable => OperationOutcome::Unreachable,
        StreetRoutingError::Timeout => OperationOutcome::Timeout,
        StreetRoutingError::RateLimited => OperationOutcome::RateLimited,
        StreetRoutingError::TemporarilyUnavailable => OperationOutcome::Unavailable,
        StreetRoutingError::MalformedResponse => OperationOutcome::MalformedResponse,
        StreetRoutingError::ResponseTooLarge { .. } => OperationOutcome::ResponseTooLarge,
    }
}
