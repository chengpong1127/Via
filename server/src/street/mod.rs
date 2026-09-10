mod fake;
mod valhalla;

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::PrimitiveDateTime;

const MAX_MATRIX_SOURCES: usize = 32;
const MAX_MATRIX_TARGETS: usize = 32;
const MAX_CORRELATION_COORDINATES: usize = 64;

pub use fake::{FakeProvider, FakeProviderBuilder};
pub use valhalla::{
    NoopOperationalEventObserver, OperationalEvent, OperationalEventObserver, RoutingOperation,
    ValhallaConfig, ValhallaProvider,
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Coordinate {
    latitude: f64,
    longitude: f64,
}

impl Coordinate {
    pub fn try_new(latitude: f64, longitude: f64) -> Result<Self, ProviderError> {
        if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
            return Err(ProviderError::InvalidRequest {
                reason: "latitude must be finite and within [-90, 90]".into(),
            });
        }
        if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
            return Err(ProviderError::InvalidRequest {
                reason: "longitude must be finite and within [-180, 180]".into(),
            });
        }
        Ok(Self {
            latitude,
            longitude,
        })
    }

    pub fn latitude(self) -> f64 {
        self.latitude
    }

    pub fn longitude(self) -> f64 {
        self.longitude
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GraphRevision(String);

impl GraphRevision {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GraphRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreetTravelMode {
    Walking,
    YouBike,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seconds(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metres(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutingTimeKind {
    DepartAt,
    ArriveBy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutingTime {
    pub kind: RoutingTimeKind,
    pub local_date_time: PrimitiveDateTime,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoutingConstraints {
    pub time: Option<RoutingTime>,
    pub max_distance: Option<Metres>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixOrientation {
    OneToMany,
    ManyToOne,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatrixQuery {
    pub orientation: MatrixOrientation,
    pub travel_mode: StreetTravelMode,
    pub sources: Vec<Coordinate>,
    pub targets: Vec<Coordinate>,
    pub constraints: RoutingConstraints,
}

impl MatrixQuery {
    fn validate(&self) -> Result<(), ProviderError> {
        if self.sources.is_empty() || self.targets.is_empty() {
            return Err(ProviderError::InvalidRequest {
                reason: "matrix sources and targets must not be empty".into(),
            });
        }
        if self.sources.len() > MAX_MATRIX_SOURCES || self.targets.len() > MAX_MATRIX_TARGETS {
            return Err(ProviderError::InvalidRequest {
                reason: "matrix exceeds Via's source or target bound".into(),
            });
        }
        match self.orientation {
            MatrixOrientation::OneToMany if self.sources.len() != 1 => {
                Err(ProviderError::InvalidRequest {
                    reason: "one-to-many matrices require exactly one source".into(),
                })
            }
            MatrixOrientation::ManyToOne if self.targets.len() != 1 => {
                Err(ProviderError::InvalidRequest {
                    reason: "many-to-one matrices require exactly one target".into(),
                })
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DetailedPathQuery {
    pub travel_mode: StreetTravelMode,
    pub origin: Coordinate,
    pub destination: Coordinate,
    pub constraints: RoutingConstraints,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Correlation {
    pub input: Coordinate,
    pub correlated: Coordinate,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MatrixReachability {
    Reachable { duration: Seconds, distance: Metres },
    Unreachable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatrixCell {
    pub source_index: usize,
    pub target_index: usize,
    pub source_correlation: Option<Coordinate>,
    pub target_correlation: Option<Coordinate>,
    pub reachability: MatrixReachability,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatrixResult {
    pub cells: Vec<Vec<MatrixCell>>,
    pub graph_revision: GraphRevision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManeuverKind {
    Depart,
    Continue,
    TurnLeft,
    TurnRight,
    UTurn,
    EnterRoundabout,
    ExitRoundabout,
    Arrive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maneuver {
    pub kind: ManeuverKind,
    pub begin_geometry_index: usize,
    pub end_geometry_index: Option<usize>,
    pub street_names: Vec<String>,
    pub roundabout_exit: Option<u32>,
}

impl Maneuver {
    pub fn depart(index: usize) -> Self {
        Self::new(ManeuverKind::Depart, index)
    }

    pub fn turn_right(index: usize) -> Self {
        Self::new(ManeuverKind::TurnRight, index)
    }

    pub fn arrive(index: usize) -> Self {
        Self::new(ManeuverKind::Arrive, index)
    }

    pub fn new(kind: ManeuverKind, begin_geometry_index: usize) -> Self {
        Self {
            kind,
            begin_geometry_index,
            end_geometry_index: None,
            street_names: Vec::new(),
            roundabout_exit: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    pub text: String,
    pub url: String,
    pub licence: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DetailedPath {
    pub duration: Seconds,
    pub distance: Metres,
    pub geometry: Vec<Coordinate>,
    pub maneuvers: Vec<Maneuver>,
    pub graph_revision: GraphRevision,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub correlation: bool,
    pub matrices: bool,
    pub detailed_paths: bool,
}

impl ProviderCapabilities {
    pub const REQUIRED: Self = Self {
        correlation: true,
        matrices: true,
        detailed_paths: true,
    };

    pub fn supports(self, required: Self) -> bool {
        (!required.correlation || self.correlation)
            && (!required.matrices || self.matrices)
            && (!required.detailed_paths || self.detailed_paths)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCategory {
    Fake,
    Valhalla,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub category: ProviderCategory,
    pub healthy: bool,
    pub graph_revision: GraphRevision,
    pub capabilities: ProviderCapabilities,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    #[error("invalid street-routing request: {reason}")]
    InvalidRequest { reason: String },
    #[error("street-routing endpoints are unreachable")]
    UnreachableEndpoints,
    #[error("street-routing provider timed out")]
    Timeout,
    #[error("street-routing provider rate limit reached")]
    RateLimited,
    #[error("street-routing provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("street-routing provider returned a malformed response")]
    MalformedResponse,
    #[error("street-routing provider lacks a required capability")]
    UnsupportedCapability,
    #[error("street graph revision changed from {expected} to {actual}")]
    InconsistentGraphRevision {
        expected: GraphRevision,
        actual: GraphRevision,
    },
    #[error("street-routing response exceeded {limit_bytes} bytes")]
    ResponseTooLarge { limit_bytes: usize },
}

#[async_trait]
pub trait StreetRoutingProvider: Send + Sync {
    async fn status(&self) -> Result<ProviderStatus, ProviderError>;

    async fn correlate(
        &self,
        graph_revision: &GraphRevision,
        coordinates: &[Coordinate],
    ) -> Result<Vec<Correlation>, ProviderError>;

    async fn matrix(
        &self,
        graph_revision: &GraphRevision,
        query: &MatrixQuery,
    ) -> Result<MatrixResult, ProviderError>;

    async fn detailed_path(
        &self,
        graph_revision: &GraphRevision,
        query: &DetailedPathQuery,
    ) -> Result<DetailedPath, ProviderError>;
}

#[derive(Clone)]
pub struct StreetRoutingSession {
    provider: Arc<dyn StreetRoutingProvider>,
    pinned_status: ProviderStatus,
}

impl StreetRoutingSession {
    pub async fn start(provider: Arc<dyn StreetRoutingProvider>) -> Result<Self, ProviderError> {
        Self::start_with_capabilities(provider, ProviderCapabilities::REQUIRED).await
    }

    pub async fn start_with_capabilities(
        provider: Arc<dyn StreetRoutingProvider>,
        required: ProviderCapabilities,
    ) -> Result<Self, ProviderError> {
        let status = provider.status().await?;
        if !status.healthy {
            return Err(ProviderError::TemporarilyUnavailable);
        }
        if !status.capabilities.supports(required) {
            return Err(ProviderError::UnsupportedCapability);
        }
        Ok(Self {
            provider,
            pinned_status: status,
        })
    }

    pub fn pinned_status(&self) -> &ProviderStatus {
        &self.pinned_status
    }

    pub async fn current_status(&self) -> Result<ProviderStatus, ProviderError> {
        self.provider.status().await
    }

    pub async fn correlate(
        &self,
        coordinates: Vec<Coordinate>,
    ) -> Result<Vec<Correlation>, ProviderError> {
        if coordinates.is_empty() || coordinates.len() > MAX_CORRELATION_COORDINATES {
            return Err(ProviderError::InvalidRequest {
                reason: "correlation requires a bounded non-empty coordinate set".into(),
            });
        }
        self.provider
            .correlate(&self.pinned_status.graph_revision, &coordinates)
            .await
    }

    pub async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, ProviderError> {
        query.validate()?;
        let result = self
            .provider
            .matrix(&self.pinned_status.graph_revision, &query)
            .await?;
        if result.graph_revision != self.pinned_status.graph_revision {
            return Err(ProviderError::InconsistentGraphRevision {
                expected: self.pinned_status.graph_revision.clone(),
                actual: result.graph_revision,
            });
        }
        Ok(result)
    }

    pub async fn detailed_path(
        &self,
        query: DetailedPathQuery,
    ) -> Result<DetailedPath, ProviderError> {
        let path = self
            .provider
            .detailed_path(&self.pinned_status.graph_revision, &query)
            .await?;
        if path.graph_revision != self.pinned_status.graph_revision {
            return Err(ProviderError::InconsistentGraphRevision {
                expected: self.pinned_status.graph_revision.clone(),
                actual: path.graph_revision,
            });
        }
        Ok(path)
    }
}
