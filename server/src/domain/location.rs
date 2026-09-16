//! Provider-neutral coordinates used by requests, transit artifacts, and journeys.

use serde::{Deserialize, Serialize};

pub(crate) const METRES_PER_DEGREE: f64 = 111_320.0;
const EARTH_RADIUS_METRES: f64 = 6_371_000.0;

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

    pub fn planar_distance_metres(self, other: Self) -> u64 {
        let latitude = ((self.latitude + other.latitude) / 2.0).to_radians();
        let north = (other.latitude - self.latitude) * METRES_PER_DEGREE;
        let east = (other.longitude - self.longitude) * METRES_PER_DEGREE * latitude.cos();
        north.hypot(east).max(0.0).round() as u64
    }

    pub fn haversine_distance_metres(self, other: Self) -> f64 {
        let from_latitude = self.latitude.to_radians();
        let to_latitude = other.latitude.to_radians();
        let delta_latitude = (other.latitude - self.latitude).to_radians();
        let delta_longitude = (other.longitude - self.longitude).to_radians();
        let haversine = (delta_latitude / 2.0).sin().powi(2)
            + from_latitude.cos() * to_latitude.cos() * (delta_longitude / 2.0).sin().powi(2);
        EARTH_RADIUS_METRES * 2.0 * haversine.sqrt().asin()
    }
}

#[cfg(test)]
mod tests {
    use super::RequestCoordinate;

    #[test]
    fn distance_helpers_preserve_the_two_coordinate_semantics() {
        let origin = RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        };
        let destination = RequestCoordinate {
            latitude: 25.001,
            longitude: 121.501,
        };

        assert_eq!(origin.planar_distance_metres(destination), 150);
        assert_eq!(
            origin.planar_distance_metres(destination),
            destination.planar_distance_metres(origin)
        );
        assert!((origin.haversine_distance_metres(destination) - 150.1).abs() < 0.1);
    }
}
