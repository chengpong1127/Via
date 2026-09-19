use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use time::{format_description::FormatItem, macros::format_description};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};
use url::Url;

use crate::{
    health::DependencyHealth,
    street::{
        Attribution, Coordinate, Maneuver, ManeuverKind, MatrixCell, MatrixLimits, MatrixQuery,
        MatrixResult, Metres, RouteQuery, RoutingConstraints, RoutingTimeKind, Seconds, StreetPath,
        StreetRouter, StreetRoutingError, StreetTravelMode,
    },
};

const OSM_ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";
const ODBL_URL: &str = "https://opendatacommons.org/licenses/odbl/1-0/";

#[derive(Clone, Debug)]
pub struct ValhallaConfig {
    pub base_url: Url,
    pub graph_revision: String,
    pub request_timeout: Duration,
    pub max_concurrency: usize,
    pub max_sources: usize,
    pub max_targets: usize,
    pub max_response_bytes: usize,
    pub expected_tileset_last_modified: u64,
}

impl ValhallaConfig {
    pub fn local(graph_revision: impl Into<String>, expected_tileset_last_modified: u64) -> Self {
        Self {
            base_url: Url::parse("http://127.0.0.1:8002/").expect("static URL is valid"),
            graph_revision: graph_revision.into(),
            request_timeout: Duration::from_millis(1_200),
            max_concurrency: 8,
            max_sources: 32,
            max_targets: 32,
            max_response_bytes: 2 * 1024 * 1024,
            expected_tileset_last_modified,
        }
    }

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
        if self.graph_revision.trim().is_empty() {
            return Err(StreetRoutingError::InvalidRequest {
                reason: "Valhalla graph revision must not be empty".into(),
            });
        }
        Ok(())
    }
}

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
    pub graph_revision: String,
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
            graph_revision: self.config.graph_revision.clone(),
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
            .await
            .map_err(map_transport_error)?;
        let wire: StatusResponse = self.parse_response(response).await?;
        if wire.tileset_last_modified != self.config.expected_tileset_last_modified {
            return Err(StreetRoutingError::TemporarilyUnavailable);
        }
        Ok(wire.has_tiles.unwrap_or(true))
    }

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
        let _permit = self.permit().await?;
        let response = self
            .client
            .post(self.endpoint("sources_to_targets")?)
            .json(&request)
            .send()
            .await
            .map_err(map_transport_error)?;
        let wire: MatrixResponse = self.parse_response(response).await?;
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

    async fn route_inner(&self, query: &RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        let request = RouteRequest {
            locations: vec![query.origin.into(), query.destination.into()],
            costing: costing(query.travel_mode),
            units: "kilometers",
            shape_format: "polyline6",
            date_time: wire_time(query.constraints)?,
        };
        let _permit = self.permit().await?;
        let response = self
            .client
            .post(self.endpoint("route")?)
            .json(&request)
            .send()
            .await
            .map_err(map_transport_error)?;
        let wire: RouteResponse = self.parse_response(response).await?;
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
        let started = Instant::now();
        let result = self.with_deadline(self.health_inner()).await;
        self.observe(RoutingOperation::Health, started, 0, 0, &result);
        result.is_ok_and(|healthy| healthy)
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

fn outcome_for(error: &StreetRoutingError) -> OperationOutcome {
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

fn costing(travel_mode: StreetTravelMode) -> &'static str {
    match travel_mode {
        StreetTravelMode::Walking => "pedestrian",
        StreetTravelMode::Bicycle => "bicycle",
    }
}

const VALHALLA_LOCAL_TIME_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]");

fn wire_time(
    constraints: RoutingConstraints,
) -> Result<Option<DateTimeRequest>, StreetRoutingError> {
    constraints
        .time
        .map(|routing_time| {
            let value = routing_time
                .local_date_time
                .format(VALHALLA_LOCAL_TIME_FORMAT)
                .map_err(|_| StreetRoutingError::InvalidRequest {
                    reason: "street-routing time could not be formatted".into(),
                })?;
            Ok(DateTimeRequest {
                kind: match routing_time.kind {
                    RoutingTimeKind::DepartAt => 1,
                    RoutingTimeKind::ArriveBy => 2,
                },
                value,
            })
        })
        .transpose()
}

fn osm_attribution() -> Vec<Attribution> {
    vec![Attribution {
        text: "© OpenStreetMap contributors".into(),
        url: OSM_ATTRIBUTION_URL.into(),
        licence: ODBL_URL.into(),
    }]
}

fn seconds(value: f64) -> Result<Seconds, StreetRoutingError> {
    if !value.is_finite() || value < 0.0 || value > f64::from(u32::MAX) {
        return Err(StreetRoutingError::MalformedResponse);
    }
    Ok(Seconds(value.round() as u32))
}

fn kilometres_to_metres(value: f64) -> Result<Metres, StreetRoutingError> {
    let metres = value * 1_000.0;
    if !metres.is_finite() || metres < 0.0 || metres > f64::from(u32::MAX) {
        return Err(StreetRoutingError::MalformedResponse);
    }
    Ok(Metres(metres.round() as u32))
}

fn normalize_matrix_cell(wire: MatrixWireCell) -> Result<MatrixCell, StreetRoutingError> {
    match (wire.time, wire.distance) {
        (Some(time), Some(distance)) => Ok(MatrixCell::Reachable {
            duration: seconds(time)?,
            distance: kilometres_to_metres(distance)?,
        }),
        (None, None) => Ok(MatrixCell::Unreachable),
        _ => Err(StreetRoutingError::MalformedResponse),
    }
}

fn normalize_maneuver(wire: RouteManeuver) -> Result<Maneuver, StreetRoutingError> {
    let kind = match wire.kind {
        1..=3 => ManeuverKind::Depart,
        4..=6 => ManeuverKind::Arrive,
        9..=11 | 18 | 20 | 23 => ManeuverKind::TurnRight,
        12 | 13 => ManeuverKind::UTurn,
        14..=16 | 19 | 21 | 24 => ManeuverKind::TurnLeft,
        26 => ManeuverKind::EnterRoundabout,
        27 => ManeuverKind::ExitRoundabout,
        _ => ManeuverKind::Continue,
    };
    Ok(Maneuver {
        kind,
        begin_geometry_index: wire.begin_shape_index,
        end_geometry_index: wire.end_shape_index,
        street_names: wire.street_names.unwrap_or_default(),
        roundabout_exit: wire.roundabout_exit_count,
    })
}

fn decode_polyline6(encoded: &str) -> Result<Vec<Coordinate>, StreetRoutingError> {
    let bytes = encoded.as_bytes();
    let mut index = 0;
    let mut latitude = 0_i64;
    let mut longitude = 0_i64;
    let mut coordinates = Vec::new();
    while index < bytes.len() {
        latitude = latitude
            .checked_add(decode_component(bytes, &mut index)?)
            .ok_or(StreetRoutingError::MalformedResponse)?;
        longitude = longitude
            .checked_add(decode_component(bytes, &mut index)?)
            .ok_or(StreetRoutingError::MalformedResponse)?;
        coordinates.push(
            Coordinate::try_new(
                latitude as f64 / 1_000_000.0,
                longitude as f64 / 1_000_000.0,
            )
            .map_err(|_| StreetRoutingError::MalformedResponse)?,
        );
    }
    Ok(coordinates)
}

fn decode_component(bytes: &[u8], index: &mut usize) -> Result<i64, StreetRoutingError> {
    let mut result = 0_u64;
    let mut shift = 0_u32;
    loop {
        let byte = *bytes
            .get(*index)
            .ok_or(StreetRoutingError::MalformedResponse)?;
        *index += 1;
        if !(63..=126).contains(&byte) || shift > 60 {
            return Err(StreetRoutingError::MalformedResponse);
        }
        let value = u64::from(byte - 63);
        result |= (value & 0x1f) << shift;
        if value < 0x20 {
            break;
        }
        shift += 5;
    }
    let signed = if result & 1 == 1 {
        !(result >> 1) as i64
    } else {
        (result >> 1) as i64
    };
    Ok(signed)
}

#[derive(Deserialize)]
struct StatusResponse {
    tileset_last_modified: u64,
    has_tiles: Option<bool>,
}

#[derive(Deserialize)]
struct ValhallaErrorResponse {
    error_code: u16,
}

#[derive(Clone, Copy, Serialize)]
struct WireLocation {
    lat: f64,
    lon: f64,
}

impl From<Coordinate> for WireLocation {
    fn from(coordinate: Coordinate) -> Self {
        Self {
            lat: coordinate.latitude(),
            lon: coordinate.longitude(),
        }
    }
}

#[derive(Serialize)]
struct MatrixRequest {
    sources: Vec<WireLocation>,
    targets: Vec<WireLocation>,
    costing: &'static str,
    units: &'static str,
    verbose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_time: Option<DateTimeRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expansion_max_distance: Option<u32>,
}

#[derive(Serialize)]
struct DateTimeRequest {
    #[serde(rename = "type")]
    kind: u8,
    value: String,
}

#[derive(Deserialize)]
struct MatrixResponse {
    sources_to_targets: Vec<Vec<MatrixWireCell>>,
}

#[derive(Deserialize)]
struct MatrixWireCell {
    time: Option<f64>,
    distance: Option<f64>,
}

#[derive(Serialize)]
struct RouteRequest {
    locations: Vec<WireLocation>,
    costing: &'static str,
    units: &'static str,
    shape_format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_time: Option<DateTimeRequest>,
}

#[derive(Deserialize)]
struct RouteResponse {
    trip: RouteTrip,
}

#[derive(Deserialize)]
struct RouteTrip {
    summary: RouteSummary,
    legs: Vec<RouteLeg>,
}

#[derive(Deserialize)]
struct RouteSummary {
    time: f64,
    length: f64,
}

#[derive(Deserialize)]
struct RouteLeg {
    shape: String,
    maneuvers: Vec<RouteManeuver>,
}

#[derive(Deserialize)]
struct RouteManeuver {
    #[serde(rename = "type")]
    kind: u16,
    begin_shape_index: usize,
    end_shape_index: Option<usize>,
    street_names: Option<Vec<String>>,
    roundabout_exit_count: Option<u32>,
}
