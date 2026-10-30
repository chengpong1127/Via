use crate::domain::bikeshare::{BikeAvailabilityError, BikeAvailabilitySource};
use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{bikeshare::BikeAvailabilitySnapshot, ids::BikeStationId};

pub mod tdx;

pub use tdx::TdxBikeAvailabilitySource;

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
