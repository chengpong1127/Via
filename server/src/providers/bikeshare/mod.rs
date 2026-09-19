//! Provider-neutral bike-share availability source and concrete adapters.

use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::bikeshare::BikeAvailabilitySnapshot;

pub mod tdx;

pub use tdx::TdxBikeAvailabilitySource;

pub const DEFAULT_BIKE_AVAILABILITY_CACHE_TTL: std::time::Duration =
    std::time::Duration::from_secs(45);

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BikeAvailabilityError {
    #[error("bike-share availability provider returned a rejected payload")]
    RejectedPayload,
    #[error("bike-share availability provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("bike-share availability provider is rate limited")]
    RateLimited,
    #[error("bike-share availability provider configuration is invalid")]
    InvalidConfiguration,
}

impl BikeAvailabilityError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::TemporarilyUnavailable | Self::RateLimited)
    }
}

#[async_trait]
pub trait BikeAvailabilitySource: Send + Sync {
    async fn snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>;
}

/// Explicitly disabled BikeShare availability for router builders that do not
/// load production BikeShare artifacts, such as offline test fixtures.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableBikeAvailabilitySource;

#[async_trait]
impl BikeAvailabilitySource for UnavailableBikeAvailabilitySource {
    async fn snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        Err(BikeAvailabilityError::InvalidConfiguration)
    }
}
