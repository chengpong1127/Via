mod events;
mod wire;

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::Instrument;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};
use url::Url;

use crate::street::{
    DependencyHealth, MatrixLimits, MatrixQuery, MatrixResult, RouteQuery, StreetPath,
    StreetRouter, StreetRoutingError,
};

use events::outcome_for;
pub use events::{
    NoopOperationalEventObserver, OperationOutcome, OperationalEvent, OperationalEventObserver,
    RoutingOperation, TracingOperationalEventObserver,
};
use wire::{
    MatrixRequest, MatrixResponse, RouteRequest, RouteResponse, StatusResponse,
    ValhallaErrorResponse, WireLocation, costing, decode_polyline6, kilometres_to_metres,
    normalize_maneuver, normalize_matrix_cell, osm_attribution, seconds, wire_time,
};

#[derive(Clone, Debug)]
pub struct ValhallaConfig {
    pub base_url: Url,
    pub request_timeout: Duration,
    pub max_concurrency: usize,
    pub max_sources: usize,
    pub max_targets: usize,
    pub max_response_bytes: usize,
}

impl ValhallaConfig {
    fn validate(&self) -> Result<(), StreetRoutingError> {
        if !matches!(self.base_url.scheme(), "http" | "https") || self.base_url.host_str().is_none()
        {
            return Err(StreetRoutingError::InvalidRequest {
                reason: "Valhalla URL must be an HTTP(S) URL with a host".into(),
            });
        }
        if self.max_concurrency == 0
            || self.max_sources == 0
            || self.max_targets == 0
            || self.max_response_bytes == 0
        {
            return Err(StreetRoutingError::InvalidRequest {
                reason: "Valhalla resource bounds must be greater than zero".into(),
            });
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct ValhallaProvider {
    config: ValhallaConfig,
    client: Client,
    concurrency: Arc<Semaphore>,
    observer: Arc<dyn OperationalEventObserver>,
}

impl ValhallaProvider {
    pub fn new(config: ValhallaConfig) -> Result<Self, StreetRoutingError> {
        Self::with_observer(config, Arc::new(NoopOperationalEventObserver))
    }

    pub fn with_observer(
        config: ValhallaConfig,
        observer: Arc<dyn OperationalEventObserver>,
    ) -> Result<Self, StreetRoutingError> {
        config.validate()?;
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|_| StreetRoutingError::InvalidRequest {
                reason: "could not construct the Valhalla HTTP client".into(),
            })?;
        Ok(Self {
            concurrency: Arc::new(Semaphore::new(config.max_concurrency)),
            config,
            client,
            observer,
        })
    }

    #[tracing::instrument(target = "via_profile", name = "Valhalla Semaphore Wait", skip_all)]
    async fn permit(&self) -> Result<OwnedSemaphorePermit, StreetRoutingError> {
        self.concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StreetRoutingError::TemporarilyUnavailable)
    }

    async fn with_deadline<T>(
        &self,
        operation: impl Future<Output = Result<T, StreetRoutingError>>,
    ) -> Result<T, StreetRoutingError> {
        timeout(self.config.request_timeout, operation)
            .await
            .unwrap_or(Err(StreetRoutingError::Timeout))
    }

    fn endpoint(&self, path: &str) -> Result<Url, StreetRoutingError> {
        self.config
            .base_url
            .join(path)
            .map_err(|_| StreetRoutingError::InvalidRequest {
                reason: "invalid Valhalla endpoint URL".into(),
            })
    }

    async fn response_bytes(&self, response: Response) -> Result<Bytes, StreetRoutingError> {
        if let Some(length) = response.content_length() {
            if length > self.config.max_response_bytes as u64 {
                return Err(StreetRoutingError::ResponseTooLarge {
                    limit_bytes: self.config.max_response_bytes,
                });
            }
        }
        let mut body = BytesMut::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_transport_error)?;
            if body.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                return Err(StreetRoutingError::ResponseTooLarge {
                    limit_bytes: self.config.max_response_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    }

    async fn parse_response<T: DeserializeOwned>(
        &self,
        response: Response,
    ) -> Result<T, StreetRoutingError> {
        let status = response.status();
        let bytes = self.response_bytes(response).await?;
        if !status.is_success() {
            let error_code = serde_json::from_slice::<ValhallaErrorResponse>(&bytes)
                .ok()
                .map(|error| error.error_code);
            return Err(map_http_error(status, error_code));
        }
        serde_json::from_slice(&bytes).map_err(|_| StreetRoutingError::MalformedResponse)
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        request: &impl Serialize,
    ) -> Result<T, StreetRoutingError> {
        let _permit = self.permit().await?;
        let response = self
            .client
            .post(self.endpoint(path)?)
            .json(request)
            .send()
            .instrument(tracing::info_span!(target: "via_profile", "Valhalla HTTP"))
            .await
            .map_err(map_transport_error)?;
        self.parse_response(response).await
    }

    fn observe<T>(
        &self,
        operation: RoutingOperation,
        started: Instant,
        source_count: usize,
        target_count: usize,
        result: &Result<T, StreetRoutingError>,
    ) {
        self.observer.observe(OperationalEvent {
            operation,
            provider: "valhalla",
            latency_milliseconds: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            outcome: result
                .as_ref()
                .map(|_| OperationOutcome::Success)
                .unwrap_or_else(outcome_for),
            source_count,
            target_count,
        });
    }

    async fn execute<T>(
        &self,
        operation: RoutingOperation,
        source_count: usize,
        target_count: usize,
        future: impl Future<Output = Result<T, StreetRoutingError>>,
    ) -> Result<T, StreetRoutingError> {
        let started = Instant::now();
        let result = self.with_deadline(future).await;
        self.observe(operation, started, source_count, target_count, &result);
        result
    }

    async fn health_inner(&self) -> Result<bool, StreetRoutingError> {
        let _permit = self.permit().await?;
        let response = self
            .client
            .get(self.endpoint("status")?)
            .send()
            .instrument(tracing::info_span!(target: "via_profile", "Valhalla HTTP"))
            .await
            .map_err(map_transport_error)?;
        let wire: StatusResponse = self.parse_response(response).await?;
        if wire.has_tiles == Some(false) {
            return Err(StreetRoutingError::TemporarilyUnavailable);
        }
        let has_routing_actions = wire.available_actions.as_ref().is_some_and(|actions| {
            actions.iter().any(|action| action == "route")
                && actions.iter().any(|action| action == "sources_to_targets")
        });
        if wire.has_tiles != Some(true) && !has_routing_actions {
            return Err(StreetRoutingError::TemporarilyUnavailable);
        }
        Ok(true)
    }

    #[tracing::instrument(target = "via_profile", name = "Valhalla Matrix", skip_all, fields(source_count = query.sources.len(), target_count = query.targets.len()))]
    async fn matrix_inner(&self, query: &MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        if query.sources.len() > self.config.max_sources
            || query.targets.len() > self.config.max_targets
        {
            return Err(StreetRoutingError::InvalidRequest {
                reason: format!(
                    "matrix exceeds source/target bounds ({}/{})",
                    self.config.max_sources, self.config.max_targets
                ),
            });
        }
        let request = MatrixRequest {
            sources: query
                .sources
                .iter()
                .copied()
                .map(WireLocation::from)
                .collect(),
            targets: query
                .targets
                .iter()
                .copied()
                .map(WireLocation::from)
                .collect(),
            costing: costing(query.travel_mode),
            units: "kilometers",
            verbose: true,
            date_time: wire_time(query.constraints)?,
            expansion_max_distance: query.constraints.max_distance.map(|value| value.0),
        };
        let wire: MatrixResponse = self.post_json("sources_to_targets", &request).await?;
        if wire.sources_to_targets.len() != query.sources.len()
            || wire
                .sources_to_targets
                .iter()
                .any(|row| row.len() != query.targets.len())
        {
            return Err(StreetRoutingError::MalformedResponse);
        }
        let cells = wire
            .sources_to_targets
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(normalize_matrix_cell)
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MatrixResult { cells })
    }

    #[tracing::instrument(target = "via_profile", name = "Valhalla Route", skip_all)]
    async fn route_inner(&self, query: &RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        let request = RouteRequest {
            locations: vec![query.origin.into(), query.destination.into()],
            costing: costing(query.travel_mode),
            units: "kilometers",
            shape_format: "polyline6",
            date_time: wire_time(query.constraints)?,
        };
        let wire: RouteResponse = self.post_json("route", &request).await?;
        let leg = wire
            .trip
            .legs
            .into_iter()
            .next()
            .ok_or(StreetRoutingError::MalformedResponse)?;
        let geometry = decode_polyline6(&leg.shape)?;
        if geometry.is_empty() {
            return Err(StreetRoutingError::MalformedResponse);
        }
        let maneuvers = leg
            .maneuvers
            .into_iter()
            .map(normalize_maneuver)
            .collect::<Result<Vec<_>, _>>()?;
        let distance = kilometres_to_metres(wire.trip.summary.length)?;
        if query
            .constraints
            .max_distance
            .is_some_and(|maximum| distance.0 > maximum.0)
        {
            return Err(StreetRoutingError::Unreachable);
        }
        Ok(StreetPath {
            duration: seconds(wire.trip.summary.time)?,
            distance,
            geometry,
            maneuvers,
            attribution: osm_attribution(),
        })
    }
}

#[async_trait]
impl DependencyHealth for ValhallaProvider {
    async fn healthy(&self) -> bool {
        self.execute(RoutingOperation::Health, 0, 0, self.health_inner())
            .await
            .is_ok_and(|healthy| healthy)
    }

    fn name(&self) -> &'static str {
        "valhalla"
    }
}

#[async_trait]
impl StreetRouter for ValhallaProvider {
    fn matrix_limits(&self) -> MatrixLimits {
        MatrixLimits {
            max_sources: self.config.max_sources,
            max_targets: self.config.max_targets,
        }
    }

    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        query.validate()?;
        self.execute(
            RoutingOperation::Matrix,
            query.sources.len(),
            query.targets.len(),
            self.matrix_inner(&query),
        )
        .await
    }

    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        self.execute(RoutingOperation::Route, 1, 1, self.route_inner(&query))
            .await
    }
}

fn map_transport_error(error: reqwest::Error) -> StreetRoutingError {
    if error.is_timeout() {
        StreetRoutingError::Timeout
    } else {
        StreetRoutingError::TemporarilyUnavailable
    }
}

fn map_http_error(status: StatusCode, error_code: Option<u16>) -> StreetRoutingError {
    if matches!(error_code, Some(170 | 171 | 172 | 210 | 211 | 440..=444)) {
        return StreetRoutingError::Unreachable;
    }
    if matches!(
        error_code,
        Some(106 | 107 | 125 | 140..=142 | 144 | 400 | 504)
    ) {
        return StreetRoutingError::InvalidRequest {
            reason: "Valhalla rejected the normalized request".into(),
        };
    }
    match status {
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            StreetRoutingError::InvalidRequest {
                reason: "Valhalla rejected the normalized request".into(),
            }
        }
        StatusCode::TOO_MANY_REQUESTS => StreetRoutingError::RateLimited,
        status if status.is_server_error() => StreetRoutingError::TemporarilyUnavailable,
        _ => StreetRoutingError::MalformedResponse,
    }
}
