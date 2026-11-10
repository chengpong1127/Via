mod fake;
pub mod offline;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::PrimitiveDateTime;

pub use crate::domain::journey::Attribution;
pub use fake::{FakeStreetRouter, FakeStreetRouterBuilder};
pub use offline::StraightLineStreetRouter;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Coordinate {
    latitude: f64,
    longitude: f64,
}

impl TryFrom<crate::domain::location::GeoCoordinate> for Coordinate {
    type Error = StreetRoutingError;

    fn try_from(value: crate::domain::location::GeoCoordinate) -> Result<Self, Self::Error> {
        Self::try_new(value.latitude, value.longitude)
    }
}

impl Coordinate {
    pub fn try_new(latitude: f64, longitude: f64) -> Result<Self, StreetRoutingError> {
        if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
            return Err(StreetRoutingError::InvalidRequest {
                reason: "latitude must be finite and within [-90, 90]".into(),
            });
        }
        if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
            return Err(StreetRoutingError::InvalidRequest {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreetTravelMode {
    Walking,
    Bicycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seconds(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Metres(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoutingTime {
    pub local_date_time: PrimitiveDateTime,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RoutingConstraints {
    pub time: Option<RoutingTime>,
    pub max_distance: Option<Metres>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixOrientation {
    OneToMany,
    ManyToOne,
    ManyToMany,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatrixLimits {
    pub max_sources: usize,
    pub max_targets: usize,
}

impl Default for MatrixLimits {
    fn default() -> Self {
        Self {
            max_sources: 32,
            max_targets: 32,
        }
    }
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
    pub fn validate(&self) -> Result<(), StreetRoutingError> {
        if self.sources.is_empty() || self.targets.is_empty() {
            return Err(StreetRoutingError::InvalidRequest {
                reason: "matrix sources and targets must not be empty".into(),
            });
        }
        match self.orientation {
            MatrixOrientation::OneToMany if self.sources.len() != 1 => {
                Err(StreetRoutingError::InvalidRequest {
                    reason: "one-to-many matrices require exactly one source".into(),
                })
            }
            MatrixOrientation::ManyToOne if self.targets.len() != 1 => {
                Err(StreetRoutingError::InvalidRequest {
                    reason: "many-to-one matrices require exactly one target".into(),
                })
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RouteQuery {
    pub travel_mode: StreetTravelMode,
    pub origin: Coordinate,
    pub destination: Coordinate,
    pub constraints: RoutingConstraints,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixCell {
    Reachable { duration: Seconds, distance: Metres },
    Unreachable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixResult {
    pub cells: Vec<Vec<MatrixCell>>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct StreetPath {
    pub duration: Seconds,
    pub distance: Metres,
    pub geometry: Vec<Coordinate>,
    pub maneuvers: Vec<Maneuver>,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StreetRoutingError {
    #[error("invalid street-routing request: {reason}")]
    InvalidRequest { reason: String },
    #[error("street-routing endpoints are unreachable")]
    Unreachable,
    #[error("street-routing provider timed out")]
    Timeout,
    #[error("street-routing provider rate limit reached")]
    RateLimited,
    #[error("street-routing provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("street-routing provider returned a malformed response")]
    MalformedResponse,
    #[error("street-routing response exceeded {limit_bytes} bytes")]
    ResponseTooLarge { limit_bytes: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFailureClass {
    Unreachable,
    Transient,
    Internal,
}

impl StreetRoutingError {
    pub fn failure_class(&self) -> ProviderFailureClass {
        match self {
            Self::Unreachable => ProviderFailureClass::Unreachable,
            Self::Timeout | Self::RateLimited | Self::TemporarilyUnavailable => {
                ProviderFailureClass::Transient
            }
            Self::InvalidRequest { .. }
            | Self::MalformedResponse
            | Self::ResponseTooLarge { .. } => ProviderFailureClass::Internal,
        }
    }
}

#[async_trait]
pub trait StreetRouter: Send + Sync {
    fn matrix_limits(&self) -> MatrixLimits {
        MatrixLimits::default()
    }

    async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError>;

    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError>;
}

#[cfg(test)]
mod coordinate_tests {
    use super::*;
    use crate::domain::location::GeoCoordinate;

    #[test]
    fn domain_conversion_preserves_coordinates_and_rejects_invalid_values() {
        for (latitude, longitude) in [(25.0, 121.5), (-90.0, -180.0), (90.0, 180.0)] {
            let coordinate = Coordinate::try_from(GeoCoordinate {
                latitude,
                longitude,
            })
            .unwrap();
            assert_eq!(coordinate.latitude(), latitude);
            assert_eq!(coordinate.longitude(), longitude);
        }
        for (latitude, longitude) in [(f64::NAN, 121.5), (91.0, 121.5), (25.0, 181.0)] {
            assert!(matches!(
                Coordinate::try_from(GeoCoordinate {
                    latitude,
                    longitude
                }),
                Err(StreetRoutingError::InvalidRequest { .. })
            ));
        }
    }
    #[test]
    fn every_street_error_has_an_explicit_failure_class() {
        let cases = [
            (
                StreetRoutingError::Unreachable,
                ProviderFailureClass::Unreachable,
            ),
            (StreetRoutingError::Timeout, ProviderFailureClass::Transient),
            (
                StreetRoutingError::RateLimited,
                ProviderFailureClass::Transient,
            ),
            (
                StreetRoutingError::TemporarilyUnavailable,
                ProviderFailureClass::Transient,
            ),
            (
                StreetRoutingError::InvalidRequest {
                    reason: "invalid".into(),
                },
                ProviderFailureClass::Internal,
            ),
            (
                StreetRoutingError::MalformedResponse,
                ProviderFailureClass::Internal,
            ),
            (
                StreetRoutingError::ResponseTooLarge { limit_bytes: 1 },
                ProviderFailureClass::Internal,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.failure_class(), expected);
        }
    }
}
