//! Static transit segment time profiles and Bus runtime estimates.

use std::collections::HashMap;

use gtfs_structures::{RawStopTime, RawTrip, Shape};

use crate::domain::{
    location::RequestCoordinate,
    transit::{
        SegmentProfile, SegmentTimeSource, SegmentTravelTime, TransitFrequency, TransitMode,
        TransitStop, TransitTrip,
    },
};

pub(super) fn segment_profile(
    pattern: &[u32],
    scheduled_trips: &[TransitTrip],
    frequencies: &[TransitFrequency],
    bus_segment_samples: &[Vec<u32>],
    mode: TransitMode,
    stops: &[TransitStop],
) -> SegmentProfile {
    let segment_count = pattern.len().saturating_sub(1);
    let mut minimums = vec![u32::MAX; segment_count];
    for times in scheduled_trips
        .iter()
        .map(|trip| trip.stop_times.as_slice())
        .chain(
            frequencies
                .iter()
                .map(|frequency| frequency.template_stop_times.as_slice()),
        )
    {
        for (minimum, pair) in minimums.iter_mut().zip(times.windows(2)) {
            let duration = pair[1]
                .arrival_seconds
                .saturating_sub(pair[0].departure_seconds);
            if duration > 0 {
                *minimum = (*minimum).min(duration);
            }
        }
    }
    let estimated = estimated_segments(pattern, mode, stops);
    let segments = minimums
        .into_iter()
        .enumerate()
        .map(|(index, scheduled)| {
            if scheduled != u32::MAX {
                SegmentTravelTime {
                    seconds: scheduled,
                    source: SegmentTimeSource::Schedule,
                }
            } else if mode == TransitMode::Bus
                && let Some(derived) = median(bus_segment_samples[index].clone())
                && derived > 0
            {
                SegmentTravelTime {
                    seconds: derived,
                    source: SegmentTimeSource::Derived,
                }
            } else {
                SegmentTravelTime {
                    seconds: estimated[index],
                    source: SegmentTimeSource::Estimated,
                }
            }
        })
        .collect();
    SegmentProfile { segments }
}

fn estimated_segments(pattern: &[u32], mode: TransitMode, stops: &[TransitStop]) -> Vec<u32> {
    let kmh = match mode {
        TransitMode::Bus => 60.0,
        TransitMode::Metro => 100.0,
        TransitMode::Rail => 160.0,
        TransitMode::HighSpeedRail => 350.0,
    };
    pattern
        .windows(2)
        .map(|pair| {
            (stops[pair[0] as usize]
                .coordinate
                .haversine_distance_metres(stops[pair[1] as usize].coordinate)
                / (kmh / 3.6))
                .ceil()
                .max(1.0) as u32
        })
        .collect()
}

pub(super) fn trip_segment_distances(
    stops: &[TransitStop],
    stop_index_by_gtfs_id: &HashMap<String, u32>,
    rows: &[&RawStopTime],
    trip: &RawTrip,
    shapes_by_id: &HashMap<&str, Vec<&Shape>>,
) -> Vec<f64> {
    let explicit = rows
        .iter()
        .map(|row| row.shape_dist_traveled.map(f64::from))
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
    if let Some(shape) = trip.shape_id.as_deref().and_then(|id| shapes_by_id.get(id))
        && let Some(distances) = stop_distances_on_shape(stops, stop_index_by_gtfs_id, rows, shape)
    {
        return distances;
    }
    rows.windows(2)
        .map(|pair| {
            let from = stops[stop_index_by_gtfs_id[pair[0].stop_id.as_str()] as usize].coordinate;
            let to = stops[stop_index_by_gtfs_id[pair[1].stop_id.as_str()] as usize].coordinate;
            from.haversine_distance_metres(to)
        })
        .collect()
}

fn stop_distances_on_shape(
    stops: &[TransitStop],
    stop_index_by_gtfs_id: &HashMap<String, u32>,
    rows: &[&RawStopTime],
    shape: &[&Shape],
) -> Option<Vec<f64>> {
    if shape.len() < 2 {
        return None;
    }
    let mut points = shape.to_vec();
    points.sort_by_key(|point| point.sequence);
    let mut cumulative = 0.0;
    let mut previous = None;
    let mut distances = Vec::with_capacity(points.len());
    for point in &points {
        if let Some(distance) = point.dist_traveled.map(f64::from).filter(|d| d.is_finite()) {
            cumulative = distance;
        } else if let Some((latitude, longitude)) = previous {
            cumulative +=
                geographic_distance_metres(latitude, longitude, point.latitude, point.longitude);
        }
        previous = Some((point.latitude, point.longitude));
        distances.push(cumulative);
    }

    let mut cursor = 0;
    let mut stop_distances = Vec::with_capacity(rows.len());
    for row in rows {
        let stop = stops[stop_index_by_gtfs_id[row.stop_id.as_str()] as usize].coordinate;
        let (index, _) =
            points
                .iter()
                .enumerate()
                .skip(cursor)
                .min_by(|(_, left), (_, right)| {
                    point_distance(stop, left).total_cmp(&point_distance(stop, right))
                })?;
        cursor = index;
        stop_distances.push(distances[index]);
    }
    let segments = stop_distances
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect::<Vec<_>>();
    valid_distances(&segments).then_some(segments)
}

fn point_distance(stop: RequestCoordinate, point: &Shape) -> f64 {
    stop.haversine_distance_metres(RequestCoordinate {
        latitude: point.latitude,
        longitude: point.longitude,
    })
}

fn geographic_distance_metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    RequestCoordinate {
        latitude: lat1,
        longitude: lon1,
    }
    .haversine_distance_metres(RequestCoordinate {
        latitude: lat2,
        longitude: lon2,
    })
}

fn valid_distances(distances: &[f64]) -> bool {
    !distances.is_empty()
        && distances
            .iter()
            .all(|distance| distance.is_finite() && *distance > 0.0)
}

pub(super) fn distribute_runtime(runtime: u32, distances: &[f64]) -> Option<Vec<u32>> {
    if runtime == 0 || distances.is_empty() || !valid_distances(distances) {
        return None;
    }
    let total_distance: f64 = distances.iter().sum();
    if !total_distance.is_finite() || total_distance <= 0.0 || runtime < distances.len() as u32 {
        return None;
    }
    let mut segments = distances
        .iter()
        .map(|distance| ((f64::from(runtime) * distance / total_distance).floor() as u32).max(1))
        .collect::<Vec<_>>();
    let allocated: u32 = segments.iter().sum();
    if allocated < runtime {
        let mut fractions = distances
            .iter()
            .enumerate()
            .map(|(index, distance)| {
                let exact = f64::from(runtime) * distance / total_distance;
                (index, exact - exact.floor())
            })
            .collect::<Vec<_>>();
        fractions.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
        for (index, _) in fractions.into_iter().take((runtime - allocated) as usize) {
            segments[index] += 1;
        }
    } else if allocated > runtime {
        let mut surplus = allocated - runtime;
        for value in segments.iter_mut().rev() {
            while surplus > 0 && *value > 1 {
                *value -= 1;
                surplus -= 1;
            }
        }
    }
    Some(segments)
}

fn median(mut values: Vec<u32>) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle])
    } else {
        Some(((u64::from(values[middle - 1]) + u64::from(values[middle])) / 2) as u32)
    }
}
