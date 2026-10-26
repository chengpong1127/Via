use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{bikeshare::BikeAvailabilitySnapshot, ids::BikeStationId};

pub mod tdx;

pub use tdx::TdxBikeAvailabilitySource;

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
    async fn snapshot(
        &self,
        station_ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableBikeAvailabilitySource;

#[async_trait]
impl BikeAvailabilitySource for UnavailableBikeAvailabilitySource {
    async fn snapshot(
        &self,
        station_ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        if station_ids.is_empty() {
            return Ok(Arc::new(BikeAvailabilitySnapshot::new(
                time::OffsetDateTime::now_utc(),
                Default::default(),
            )));
        }
        Err(BikeAvailabilityError::InvalidConfiguration)
    }
}
