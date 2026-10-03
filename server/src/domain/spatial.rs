use rstar::{RTree, primitives::GeomWithData};

use super::{
    bikeshare::BikeStation,
    location::{EARTH_RADIUS_METRES, GeoCoordinate},
    transit::TransitStop,
};

pub(crate) trait SpatialPoint {
    fn coordinate(&self) -> GeoCoordinate;
    fn stable_id(&self) -> &str;
}

impl SpatialPoint for TransitStop {
    fn coordinate(&self) -> GeoCoordinate {
        self.coordinate
    }

    fn stable_id(&self) -> &str {
        self.id.as_str()
    }
}

impl SpatialPoint for BikeStation {
    fn coordinate(&self) -> GeoCoordinate {
        self.coordinate
    }

    fn stable_id(&self) -> &str {
        self.id.as_str()
    }
}

/// Index of positions in an immutable point slice. Queries must use the same slice
/// that was indexed. Third-party geometry stays inside this module and location.
#[derive(Clone, Debug, Default)]
pub(crate) struct SpatialPointIndex {
    tree: RTree<GeomWithData<[f64; 3], usize>>,
}

// Unit-sphere coordinate conversion and chord calculations can differ by a few
// ulps. Expand candidate bounds by this conservative absolute chord tolerance
// (under a micrometre on Earth); final geographic filtering stays exact.
const CHORD_PADDING: f64 = 64.0 * f64::EPSILON;

impl SpatialPointIndex {
    pub(crate) fn build(points: &[impl SpatialPoint]) -> Self {
        Self {
            tree: RTree::bulk_load(
                points
                    .iter()
                    .enumerate()
                    .map(|(index, point)| {
                        GeomWithData::new(point.coordinate().unit_sphere_point(), index)
                    })
                    .collect(),
            ),
        }
    }

    pub(crate) fn within_radius(
        &self,
        points: &[impl SpatialPoint],
        coordinate: GeoCoordinate,
        radius_metres: f64,
    ) -> Vec<usize> {
        let angle = (radius_metres / EARTH_RADIUS_METRES).min(std::f64::consts::PI);
        let chord = 2.0 * (angle / 2.0).sin();
        let mut candidates = self
            .tree
            .locate_within_distance(
                coordinate.unit_sphere_point(),
                (chord + CHORD_PADDING).powi(2),
            )
            .map(|point| {
                let index = point.data;
                (
                    index,
                    coordinate.distance_metres(points[index].coordinate()),
                )
            })
            .filter(|(_, distance)| *distance <= radius_metres)
            .collect::<Vec<_>>();
        candidates.sort_by(|(left, left_distance), (right, right_distance)| {
            left_distance
                .total_cmp(right_distance)
                .then_with(|| points[*left].stable_id().cmp(points[*right].stable_id()))
        });
        candidates.into_iter().map(|(index, _)| index).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::BoardingPointId;

    #[test]
    fn exact_radius_boundary_is_inclusive_without_rounding() {
        let origin = GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        };
        let destination = GeoCoordinate {
            latitude: 25.001,
            longitude: 121.501,
        };
        let stops = vec![TransitStop::new(
            BoardingPointId::new("boundary"),
            "boundary",
            destination,
        )];
        let index = SpatialPointIndex::build(&stops);
        let distance = origin.distance_metres(destination);
        assert_eq!(index.within_radius(&stops, origin, distance), vec![0]);
        assert!(
            index
                .within_radius(&stops, origin, distance - 0.000_001)
                .is_empty()
        );
        assert_eq!(
            index.within_radius(&stops, origin, distance + 0.000_001),
            vec![0]
        );
    }
}
