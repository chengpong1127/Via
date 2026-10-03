use geo::{Distance, HaversineMeasure, Point};
use serde::{Deserialize, Serialize};

// Preserve the sphere used by existing routing distance estimates.
pub(crate) const EARTH_RADIUS_METRES: f64 = 6_371_000.0;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = Coordinate)]
#[serde(deny_unknown_fields)]
pub struct GeoCoordinate {
    #[schema(minimum = -90.0, maximum = 90.0)]
    pub latitude: f64,
    #[schema(minimum = -180.0, maximum = 180.0)]
    pub longitude: f64,
}

impl GeoCoordinate {
    pub fn validate(&self) -> bool {
        self.latitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && self.longitude.is_finite()
            && (-180.0..=180.0).contains(&self.longitude)
    }

    /// Spherical great-circle distance in metres.
    pub fn distance_metres(self, other: Self) -> f64 {
        HaversineMeasure::new(EARTH_RADIUS_METRES).distance(self.point(), other.point())
    }

    fn point(self) -> Point<f64> {
        Point::new(self.longitude, self.latitude)
    }

    pub(crate) fn unit_sphere_point(self) -> [f64; 3] {
        let (sin_latitude, cos_latitude) = self.latitude.to_radians().sin_cos();
        let (sin_longitude, cos_longitude) = self.longitude.to_radians().sin_cos();
        [
            cos_latitude * cos_longitude,
            cos_latitude * sin_longitude,
            sin_latitude,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::GeoCoordinate;

    #[test]
    fn distance_is_symmetric_and_matches_a_short_taiwan_baseline() {
        let origin = GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        };
        let destination = GeoCoordinate {
            latitude: 25.001,
            longitude: 121.501,
        };
        assert!((origin.distance_metres(destination) - 150.067).abs() < 0.001);
        assert_eq!(
            origin.distance_metres(destination),
            destination.distance_metres(origin)
        );
        assert_eq!(origin.distance_metres(origin), 0.0);
    }
}
