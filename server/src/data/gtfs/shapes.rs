//! Shape parsing and geometry/distance helpers.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::Path,
};

use zip::ZipArchive;

use crate::{
    data::DataError,
    domain::{location::RequestCoordinate, transit::TransitStop},
};

use super::{
    diagnostics::GtfsBuildDiagnostics,
    reader::{column, csv_error, csv_reader, optional, required},
    schedule::{CompactStopTime, TripMeta},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct ShapePoint {
    pub(super) latitude: f64,
    pub(super) longitude: f64,
    pub(super) distance: f64,
}

pub(super) fn load_shapes(
    archive: &mut ZipArchive<File>,
    path: &Path,
    trips: &[TripMeta],
    diagnostics: &mut GtfsBuildDiagnostics,
) -> Result<HashMap<String, Vec<ShapePoint>>, DataError> {
    if !archive.file_names().any(|name| name == "shapes.txt") {
        return Ok(HashMap::new());
    }
    let used = trips
        .iter()
        .filter_map(|trip| trip.shape_id.as_deref())
        .collect::<HashSet<_>>();
    let mut reader = csv_reader(archive, path, "shapes.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "shapes.txt", error))?
        .clone();
    let shape_id = column(&headers, "shape_id", path, "shapes.txt")?;
    let lat = column(&headers, "shape_pt_lat", path, "shapes.txt")?;
    let lon = column(&headers, "shape_pt_lon", path, "shapes.txt")?;
    let sequence = column(&headers, "shape_pt_sequence", path, "shapes.txt")?;
    let traveled = headers
        .iter()
        .position(|field| field == "shape_dist_traveled");
    let mut shapes = HashMap::<String, Vec<(u32, f64, f64, Option<f64>)>>::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "shapes.txt", error))?;
        let id = required(&row, shape_id);
        if !used.contains(id) {
            continue;
        }
        let Some(latitude) = required(&row, lat).parse::<f64>().ok() else {
            continue;
        };
        let Some(longitude) = required(&row, lon).parse::<f64>().ok() else {
            continue;
        };
        let Some(sequence) = required(&row, sequence).parse::<u32>().ok() else {
            continue;
        };
        let distance = optional(&row, traveled).parse::<f64>().ok();
        shapes
            .entry(id.to_owned())
            .or_default()
            .push((sequence, latitude, longitude, distance));
        diagnostics.shape_rows_loaded += 1;
    }
    Ok(shapes
        .into_iter()
        .map(|(id, mut points)| {
            points.sort_by_key(|point| point.0);
            let mut cumulative = 0.0;
            let mut previous = None;
            let points = points
                .into_iter()
                .map(|(_, latitude, longitude, explicit)| {
                    if let Some(distance) = explicit.filter(|distance| distance.is_finite()) {
                        cumulative = distance;
                    } else if let Some((previous_latitude, previous_longitude)) = previous {
                        cumulative += geographic_distance_metres(
                            previous_latitude,
                            previous_longitude,
                            latitude,
                            longitude,
                        );
                    }
                    previous = Some((latitude, longitude));
                    ShapePoint {
                        latitude,
                        longitude,
                        distance: cumulative,
                    }
                })
                .collect();
            (id, points)
        })
        .collect())
}

pub(super) fn trip_segment_distances(
    stops: &[TransitStop],
    rows: &[CompactStopTime],
    trip: &TripMeta,
    shapes: &HashMap<String, Vec<ShapePoint>>,
) -> Vec<f64> {
    let explicit = rows
        .iter()
        .map(|row| row.shape_distance)
        .collect::<Option<Vec<_>>>();
    if let Some(values) = explicit {
        let distances = values
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>();
        if valid_distances(&distances) {
            return distances;
        }
    }
    if let Some(shape) = trip.shape_id.as_ref().and_then(|id| shapes.get(id)) {
        if let Some(distances) = stop_distances_on_shape(stops, rows, shape) {
            return distances;
        }
    }
    rows.windows(2)
        .map(|pair| {
            let from = stops[pair[0].stop_index as usize].coordinate;
            let to = stops[pair[1].stop_index as usize].coordinate;
            from.haversine_distance_metres(to)
        })
        .collect()
}

pub(super) fn stop_distances_on_shape(
    stops: &[TransitStop],
    rows: &[CompactStopTime],
    shape: &[ShapePoint],
) -> Option<Vec<f64>> {
    if shape.len() < 2 {
        return None;
    }
    let mut cursor = 0;
    let mut distances = Vec::with_capacity(rows.len());
    for row in rows {
        let stop = stops[row.stop_index as usize].coordinate;
        let (index, _) =
            shape
                .iter()
                .enumerate()
                .skip(cursor)
                .min_by(|(_, left), (_, right)| {
                    point_distance(stop, left.latitude, left.longitude).total_cmp(&point_distance(
                        stop,
                        right.latitude,
                        right.longitude,
                    ))
                })?;
        cursor = index;
        distances.push(shape[index].distance);
    }
    let segment_distances = distances
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect::<Vec<_>>();
    valid_distances(&segment_distances).then_some(segment_distances)
}

fn geographic_distance_metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let radians = std::f64::consts::PI / 180.0;
    let (lat1, lat2) = (lat1 * radians, lat2 * radians);
    let delta_latitude = lat2 - lat1;
    let delta_longitude = (lon2 - lon1) * radians;
    let a = (delta_latitude / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * (delta_longitude / 2.0).sin().powi(2);
    6_371_000.0 * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

fn point_distance(stop: RequestCoordinate, latitude: f64, longitude: f64) -> f64 {
    stop.haversine_distance_metres(RequestCoordinate {
        latitude,
        longitude,
    })
}

pub(super) fn valid_distances(distances: &[f64]) -> bool {
    !distances.is_empty()
        && distances
            .iter()
            .all(|distance| distance.is_finite() && *distance > 0.0)
}
