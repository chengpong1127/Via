//! Provider-neutral coordinates used by requests, transit artifacts, and journeys.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = Coordinate)]
#[serde(deny_unknown_fields)]
pub struct RequestCoordinate {
    #[schema(minimum = -90.0, maximum = 90.0)]
    pub latitude: f64,
    #[schema(minimum = -180.0, maximum = 180.0)]
    pub longitude: f64,
}

impl RequestCoordinate {
    pub fn validate(&self) -> bool {
        self.latitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && self.longitude.is_finite()
            && (-180.0..=180.0).contains(&self.longitude)
    }
}
