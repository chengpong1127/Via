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

use super::{
    Attribution, Coordinate, Correlation, DetailedPath, DetailedPathQuery, GraphRevision, Maneuver,
    ManeuverKind, MatrixCell, MatrixQuery, MatrixReachability, MatrixResult, Metres,
    ProviderCapabilities, ProviderCategory, ProviderError, ProviderStatus, RoutingConstraints,
    RoutingTimeKind, Seconds, StreetRoutingProvider, StreetTravelMode,
};

const OSM_ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";
const ODBL_URL: &str = "https://opendatacommons.org/licenses/odbl/1-0/";

#[derive(Clone, Debug)]
pub struct ValhallaConfig {
    pub base_url: Url,
    pub graph_revision: GraphRevision,
    pub request_timeout: Duration,
    pub max_concurrency: usize,
    pub max_sources: usize,
    pub max_targets: usize,
    pub max_correlation_locations: usize,
    pub max_response_bytes: usize,
    pub expected_tileset_last_modified: u64,
}

impl ValhallaConfig {
    pub fn local(graph_revision: GraphRevision, expected_tileset_last_modified: u64) -> Self {
        Self {
            base_url: Url::parse("http://127.0.0.1:8002/").expect("static URL is valid"),
            graph_revision,
            request_timeout: Duration::from_millis(1_200),
            max_concurrency: 8,
            max_sources: 32,
            max_targets: 32,
            max_correlation_locations: 64,
            max_response_bytes: 2 * 1024 * 1024,
            expected_tileset_last_modified,
        }
    }

    fn validate(&self) -> Result<(), ProviderError> {
        if self.max_concurrency == 0
            || self.max_sources == 0
            || self.max_targets == 0
            || self.max_correlation_locations == 0
            || self.max_response_bytes == 0
        {
            return Err(ProviderError::InvalidRequest {
                reason: "Valhalla resource bounds must be greater than zero".into(),
            });
        }
        if self.graph_revision.as_str().trim().is_empty() {
            return Err(ProviderError::InvalidRequest {
                reason: "street graph revision must not be empty".into(),
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
    UnsupportedCapability,
    InconsistentRevision,
    ResponseTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingOperation {
    Health,
    Correlate,
    Matrix,
    DetailedPath,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OperationalEvent {
    pub operation: RoutingOperation,
    pub provider: ProviderCategory,
    pub graph_revision: GraphRevision,
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
    pub fn new(config: ValhallaConfig) -> Result<Self, ProviderError> {
        Self::with_observer(config, Arc::new(NoopOperationalEventObserver))
    }

    pub fn with_observer(
        config: ValhallaConfig,
        observer: Arc<dyn OperationalEventObserver>,
    ) -> Result<Self, ProviderError> {
        config.validate()?;
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|_| ProviderError::InvalidRequest {
                reason: "could not construct the Valhalla HTTP client".into(),
            })?;
        Ok(Self {
            concurrency: Arc::new(Semaphore::new(config.max_concurrency)),
            config,
            client,
            observer,
        })
    }

    async fn permit(&self) -> Result<OwnedSemaphorePermit, ProviderError> {
        self.concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ProviderError::TemporarilyUnavailable)
    }

    async fn with_deadline<T>(
        &self,
        operation: impl Future<Output = Result<T, ProviderError>>,
    ) -> Result<T, ProviderError> {
        timeout(self.config.request_timeout, operation)
            .await
            .unwrap_or(Err(ProviderError::Timeout))
    }

    fn endpoint(&self, path: &str) -> Result<Url, ProviderError> {
        self.config
            .base_url
            .join(path)
            .map_err(|_| ProviderError::InvalidRequest {
                reason: "invalid Valhalla endpoint URL".into(),
            })
    }

    async fn response_bytes(&self, response: Response) -> Result<Bytes, ProviderError> {
        if let Some(length) = response.content_length() {
            if length > self.config.max_response_bytes as u64 {
                return Err(ProviderError::ResponseTooLarge {
                    limit_bytes: self.config.max_response_bytes,
                });
            }
        }
        let mut body = BytesMut::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_transport_error)?;
            if body.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                return Err(ProviderError::ResponseTooLarge {
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
    ) -> Result<T, ProviderError> {
        let status = response.status();
        let bytes = self.response_bytes(response).await?;
        if !status.is_success() {
            let error_code = serde_json::from_slice::<ValhallaErrorResponse>(&bytes)
                .ok()
                .map(|error| error.error_code);
            return Err(map_http_error(status, error_code));
        }
        serde_json::from_slice(&bytes).map_err(|_| ProviderError::MalformedResponse)
    }

    fn ensure_revision(&self, expected: &GraphRevision) -> Result<(), ProviderError> {
        if expected != &self.config.graph_revision {
            return Err(ProviderError::InconsistentGraphRevision {
                expected: expected.clone(),
                actual: self.config.graph_revision.clone(),
            });
        }
        Ok(())
    }

    fn observe<T>(
        &self,
        operation: RoutingOperation,
        started: Instant,
        source_count: usize,
        target_count: usize,
        result: &Result<T, ProviderError>,
    ) {
        self.observer.observe(OperationalEvent {
            operation,
            provider: ProviderCategory::Valhalla,
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
        graph_revision: &GraphRevision,
        source_count: usize,
        target_count: usize,
        future: impl Future<Output = Result<T, ProviderError>>,
    ) -> Result<T, ProviderError> {
        let started = Instant::now();
        let result = match self.ensure_revision(graph_revision) {
            Ok(()) => self.with_deadline(future).await,
            Err(error) => Err(error),
        };
        self.observe(operation, started, source_count, target_count, &result);
        result
    }

    async fn status_inner(&self) -> Result<ProviderStatus, ProviderError> {
        let _permit = self.permit().await?;
        let response = self
            .client
            .get(self.endpoint("status")?)
            .send()
            .await
            .map_err(map_transport_error)?;
        let wire: StatusResponse = self.parse_response(response).await?;
        if wire.tileset_last_modified != self.config.expected_tileset_last_modified {
            return Err(ProviderError::InconsistentGraphRevision {
                expected: self.config.graph_revision.clone(),
                actual: GraphRevision::new(format!(
                    "tileset-last-modified-{}",
                    wire.tileset_last_modified
                )),
            });
        }
        Ok(ProviderStatus {
            category: ProviderCategory::Valhalla,
            healthy: wire.has_tiles.unwrap_or(true),
            graph_revision: self.config.graph_revision.clone(),
            capabilities: ProviderCapabilities::REQUIRED,
            attribution: osm_attribution(),
        })
    }

    async fn correlate_inner(
        &self,
        coordinates: &[Coordinate],
    ) -> Result<Vec<Correlation>, ProviderError> {
        if coordinates.is_empty() || coordinates.len() > self.config.max_correlation_locations {
            return Err(ProviderError::InvalidRequest {
                reason: format!(
                    "correlation requires 1..={} coordinates",
                    self.config.max_correlation_locations
                ),
            });
        }
        let request = LocateRequest {
            locations: coordinates
                .iter()
                .copied()
                .map(WireLocation::from)
                .collect(),
            verbose: false,
        };
        let _permit = self.permit().await?;
        let response = self
            .client
            .post(self.endpoint("locate")?)
            .json(&request)
            .send()
            .await
            .map_err(map_transport_error)?;
        let wire: Vec<LocateResult> = self.parse_response(response).await?;
        if wire.len() != coordinates.len() {
            return Err(ProviderError::MalformedResponse);
        }
        wire.into_iter()
            .zip(coordinates.iter().copied())
            .map(|(result, input)| {
                let edge = result
                    .edges
                    .into_iter()
                    .next()
                    .ok_or(ProviderError::UnreachableEndpoints)?;
                let correlated = Coordinate::try_new(edge.correlated_lat, edge.correlated_lon)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                Ok(Correlation { input, correlated })
            })
            .collect()
    }

    async fn matrix_inner(&self, query: &MatrixQuery) -> Result<MatrixResult, ProviderError> {
        if query.sources.len() > self.config.max_sources
            || query.targets.len() > self.config.max_targets
        {
            return Err(ProviderError::InvalidRequest {
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
            return Err(ProviderError::MalformedResponse);
        }
        let cells = wire
            .sources_to_targets
            .into_iter()
            .enumerate()
            .map(|(source_index, row)| {
                row.into_iter()
                    .enumerate()
                    .map(|(target_index, cell)| {
                        normalize_matrix_cell(source_index, target_index, cell)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MatrixResult {
            cells,
            graph_revision: self.config.graph_revision.clone(),
        })
    }

    async fn detailed_path_inner(
        &self,
        query: &DetailedPathQuery,
    ) -> Result<DetailedPath, ProviderError> {
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
            .ok_or(ProviderError::MalformedResponse)?;
        let geometry = decode_polyline6(&leg.shape)?;
        if geometry.is_empty() {
            return Err(ProviderError::MalformedResponse);
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
            return Err(ProviderError::UnreachableEndpoints);
        }
        Ok(DetailedPath {
            duration: seconds(wire.trip.summary.time)?,
            distance,
            geometry,
            maneuvers,
            graph_revision: self.config.graph_revision.clone(),
            attribution: osm_attribution(),
        })
    }
}

#[async_trait]
impl StreetRoutingProvider for ValhallaProvider {
    async fn status(&self) -> Result<ProviderStatus, ProviderError> {
        let started = Instant::now();
        let result = self.with_deadline(self.status_inner()).await;
        self.observe(RoutingOperation::Health, started, 0, 0, &result);
        result
    }

    async fn correlate(
        &self,
        graph_revision: &GraphRevision,
        coordinates: &[Coordinate],
    ) -> Result<Vec<Correlation>, ProviderError> {
        self.execute(
            RoutingOperation::Correlate,
            graph_revision,
            coordinates.len(),
            0,
            self.correlate_inner(coordinates),
        )
        .await
    }

    async fn matrix(
        &self,
        graph_revision: &GraphRevision,
        query: &MatrixQuery,
    ) -> Result<MatrixResult, ProviderError> {
        self.execute(
            RoutingOperation::Matrix,
            graph_revision,
            query.sources.len(),
            query.targets.len(),
            self.matrix_inner(query),
        )
        .await
    }

    async fn detailed_path(
        &self,
        graph_revision: &GraphRevision,
        query: &DetailedPathQuery,
    ) -> Result<DetailedPath, ProviderError> {
        self.execute(
            RoutingOperation::DetailedPath,
            graph_revision,
            1,
            1,
            self.detailed_path_inner(query),
        )
        .await
    }
}

fn map_transport_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::TemporarilyUnavailable
    }
}

fn map_http_error(status: StatusCode, error_code: Option<u16>) -> ProviderError {
    if matches!(error_code, Some(170 | 171 | 172 | 210 | 211 | 440..=444)) {
        return ProviderError::UnreachableEndpoints;
    }
    if matches!(
        error_code,
        Some(106 | 107 | 125 | 140..=142 | 144 | 400 | 504)
    ) {
        return ProviderError::UnsupportedCapability;
    }
    match status {
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            ProviderError::InvalidRequest {
                reason: "Valhalla rejected the normalized request".into(),
            }
        }
        StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimited,
        status if status.is_server_error() => ProviderError::TemporarilyUnavailable,
        _ => ProviderError::MalformedResponse,
    }
}

fn outcome_for(error: &ProviderError) -> OperationOutcome {
    match error {
        ProviderError::InvalidRequest { .. } => OperationOutcome::InvalidRequest,
        ProviderError::UnreachableEndpoints => OperationOutcome::Unreachable,
        ProviderError::Timeout => OperationOutcome::Timeout,
        ProviderError::RateLimited => OperationOutcome::RateLimited,
        ProviderError::TemporarilyUnavailable => OperationOutcome::Unavailable,
        ProviderError::MalformedResponse => OperationOutcome::MalformedResponse,
        ProviderError::UnsupportedCapability => OperationOutcome::UnsupportedCapability,
        ProviderError::InconsistentGraphRevision { .. } => OperationOutcome::InconsistentRevision,
        ProviderError::ResponseTooLarge { .. } => OperationOutcome::ResponseTooLarge,
    }
}

fn costing(travel_mode: StreetTravelMode) -> &'static str {
    match travel_mode {
        StreetTravelMode::Walking => "pedestrian",
        StreetTravelMode::YouBike => "bicycle",
    }
}

const VALHALLA_LOCAL_TIME_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]");

fn wire_time(constraints: RoutingConstraints) -> Result<Option<DateTimeRequest>, ProviderError> {
    constraints
        .time
        .map(|routing_time| {
            let value = routing_time
                .local_date_time
                .format(VALHALLA_LOCAL_TIME_FORMAT)
                .map_err(|_| ProviderError::InvalidRequest {
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

fn seconds(value: f64) -> Result<Seconds, ProviderError> {
    if !value.is_finite() || value < 0.0 || value > f64::from(u32::MAX) {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(Seconds(value.round() as u32))
}

fn kilometres_to_metres(value: f64) -> Result<Metres, ProviderError> {
    let metres = value * 1_000.0;
    if !metres.is_finite() || metres < 0.0 || metres > f64::from(u32::MAX) {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(Metres(metres.round() as u32))
}

fn normalize_matrix_cell(
    source_index: usize,
    target_index: usize,
    wire: MatrixWireCell,
) -> Result<MatrixCell, ProviderError> {
    let reachability = match (wire.time, wire.distance) {
        (Some(time), Some(distance)) => MatrixReachability::Reachable {
            duration: seconds(time)?,
            distance: kilometres_to_metres(distance)?,
        },
        (None, None) => MatrixReachability::Unreachable,
        _ => return Err(ProviderError::MalformedResponse),
    };
    Ok(MatrixCell {
        source_index,
        target_index,
        source_correlation: optional_coordinate(wire.begin_lat, wire.begin_lon)?,
        target_correlation: optional_coordinate(wire.end_lat, wire.end_lon)?,
        reachability,
    })
}

fn optional_coordinate(
    latitude: Option<f64>,
    longitude: Option<f64>,
) -> Result<Option<Coordinate>, ProviderError> {
    match (latitude, longitude) {
        (Some(latitude), Some(longitude)) => Coordinate::try_new(latitude, longitude)
            .map(Some)
            .map_err(|_| ProviderError::MalformedResponse),
        (None, None) => Ok(None),
        _ => Err(ProviderError::MalformedResponse),
    }
}

fn normalize_maneuver(wire: RouteManeuver) -> Result<Maneuver, ProviderError> {
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

fn decode_polyline6(encoded: &str) -> Result<Vec<Coordinate>, ProviderError> {
    let bytes = encoded.as_bytes();
    let mut index = 0;
    let mut latitude = 0_i64;
    let mut longitude = 0_i64;
    let mut coordinates = Vec::new();
    while index < bytes.len() {
        latitude = latitude
            .checked_add(decode_component(bytes, &mut index)?)
            .ok_or(ProviderError::MalformedResponse)?;
        longitude = longitude
            .checked_add(decode_component(bytes, &mut index)?)
            .ok_or(ProviderError::MalformedResponse)?;
        coordinates.push(
            Coordinate::try_new(
                latitude as f64 / 1_000_000.0,
                longitude as f64 / 1_000_000.0,
            )
            .map_err(|_| ProviderError::MalformedResponse)?,
        );
    }
    Ok(coordinates)
}

fn decode_component(bytes: &[u8], index: &mut usize) -> Result<i64, ProviderError> {
    let mut result = 0_u64;
    let mut shift = 0_u32;
    loop {
        let byte = *bytes.get(*index).ok_or(ProviderError::MalformedResponse)?;
        *index += 1;
        if !(63..=126).contains(&byte) || shift > 60 {
            return Err(ProviderError::MalformedResponse);
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
    #[allow(dead_code)]
    version: String,
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
struct LocateRequest {
    locations: Vec<WireLocation>,
    verbose: bool,
}

#[derive(Deserialize)]
struct LocateResult {
    edges: Vec<LocateEdge>,
}

#[derive(Deserialize)]
struct LocateEdge {
    correlated_lat: f64,
    correlated_lon: f64,
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
    begin_lat: Option<f64>,
    begin_lon: Option<f64>,
    end_lat: Option<f64>,
    end_lon: Option<f64>,
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
