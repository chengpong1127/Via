//! Derive static per-segment timing profiles and fallback costs.

use crate::domain::transit::{
    ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime, TransitMode,
    TransitStop,
};

use super::shapes::valid_distances;

pub(super) fn profile_for_builder(
    pattern: &[u32],
    scheduled_trips: &[crate::domain::transit::TransitTrip],
    frequencies: &[crate::domain::transit::TransitFrequency],
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
            let from = stops[pair[0] as usize].coordinate;
            let to = stops[pair[1] as usize].coordinate;
            (from.haversine_distance_metres(to) / (kmh / 3.6))
                .ceil()
                .max(1.0) as u32
        })
        .collect()
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

pub(super) fn median(mut values: Vec<u32>) -> Option<u32> {
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

pub(super) fn is_chronological_times(times: &[ScheduledStopTime]) -> bool {
    !times.is_empty()
        && times
            .iter()
            .all(|time| time.arrival_seconds <= time.departure_seconds)
        && times
            .windows(2)
            .all(|window| window[0].departure_seconds <= window[1].arrival_seconds)
}
