use async_trait::async_trait;
use std::sync::Arc;

use crate::domain::{
    ids::EffectiveTripId,
    transit::{MetroFrequency, MetroTiming, TransitMode, TransitNetwork},
    transit_timing::{
        DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, TimingReliability,
    },
};

use super::source::TransitDepartureSource;

/// Materializes Metro departures from station-level schedules or headways.
///
/// Metro's static TDX data describes a station timetable/frequency and a
/// separate inter-station travel-time table. It is therefore deliberately not
/// represented as a collection of `ScheduledTrip` values.
#[derive(Clone)]
pub struct MetroDepartureSource {
    network: Arc<TransitNetwork>,
}

impl MetroDepartureSource {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        Self { network }
    }

    fn candidates(
        &self,
        timing: &MetroTiming,
        board_stop_id: &crate::domain::ids::BoardingPointId,
        service_day_start: time::OffsetDateTime,
        after: time::OffsetDateTime,
        limit: usize,
    ) -> Vec<(
        time::OffsetDateTime,
        crate::domain::transit_timing::TimingBasis,
    )> {
        let Some(station_timing) = timing
            .station_timings
            .iter()
            .find(|station| &station.stop_id == board_stop_id)
        else {
            return Vec::new();
        };
        let after_seconds =
            u32::try_from((after - service_day_start).whole_seconds().max(0)).unwrap_or(u32::MAX);
        let mut candidates = station_timing
            .departures_seconds
            .iter()
            .copied()
            .map(|seconds| {
                (
                    service_day_start + time::Duration::seconds(i64::from(seconds)),
                    crate::domain::transit_timing::TimingBasis::Schedule,
                )
            })
            .chain(
                station_timing
                    .frequencies
                    .iter()
                    .flat_map(|frequency| expand_frequency(frequency, after_seconds, limit))
                    .map(|seconds| {
                        (
                            service_day_start + time::Duration::seconds(i64::from(seconds)),
                            crate::domain::transit_timing::TimingBasis::FrequencyEstimate,
                        )
                    }),
            )
            .filter(|(departure_at, _)| *departure_at >= after)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(timing_basis_order(left.1).cmp(&timing_basis_order(right.1)))
        });
        candidates.dedup_by(|left, right| left.0 == right.0);
        candidates.truncate(limit);
        candidates
    }

    fn effective_trip(
        &self,
        service_id: &crate::domain::ids::TransitServiceId,
        timing: &MetroTiming,
        departure_at: time::OffsetDateTime,
        timing_basis: crate::domain::transit_timing::TimingBasis,
        board_stop_id: &crate::domain::ids::BoardingPointId,
    ) -> Option<EffectiveTrip> {
        let service = self.network.service(service_id)?;
        let board_index = service
            .pattern
            .iter()
            .position(|stop_id| stop_id == board_stop_id)?;
        if (!timing.pattern.is_empty() && timing.pattern != service.pattern)
            || timing.inter_station_travel_seconds.len() != service.pattern.len() - 1
            || timing.inter_station_travel_seconds.contains(&0)
        {
            return None;
        }

        let mut offsets = vec![0i64; service.pattern.len()];
        for index in (board_index + 1)..service.pattern.len() {
            offsets[index] = offsets[index - 1]
                .checked_add(i64::from(timing.inter_station_travel_seconds[index - 1]))?;
        }
        for index in (0..board_index).rev() {
            offsets[index] = offsets[index + 1]
                .checked_sub(i64::from(timing.inter_station_travel_seconds[index]))?;
        }
        let stop_times = service
            .pattern
            .iter()
            .zip(offsets)
            .map(|(stop_id, offset)| {
                let at = departure_at + time::Duration::seconds(offset);
                EffectiveStopTime {
                    stop_id: stop_id.clone(),
                    arrival_at: at,
                    departure_at: at,
                }
            })
            .collect::<Vec<_>>();
        if stop_times.windows(2).any(|times| {
            times[0].departure_at >= times[1].arrival_at
                || times[0].arrival_at > times[0].departure_at
                || times[1].arrival_at > times[1].departure_at
        }) {
            return None;
        }
        Some(EffectiveTrip {
            id: EffectiveTripId::new(format!(
                "metro:{}:{}:{}",
                service_id.as_str(),
                board_stop_id.as_str(),
                departure_at.unix_timestamp_nanos()
            )),
            service_id: service_id.clone(),
            scheduled_trip_id: None,
            stop_times,
            timing_basis,
            timing_reliability: TimingReliability::Fallback,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        })
    }
}

#[async_trait]
impl TransitDepartureSource for MetroDepartureSource {
    fn mode(&self) -> TransitMode {
        TransitMode::Metro
    }

    fn static_timing_reliability(&self, _query: &DepartureQuery) -> TimingReliability {
        TimingReliability::Fallback
    }

    async fn static_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let Some(service) = self.network.service(&query.service_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if service.transit_mode != TransitMode::Metro
            || !service.pattern.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        let Some(timing) = self
            .network
            .metro_timings
            .iter()
            .find(|timing| timing.service_id == query.service_id)
        else {
            return Ok(Vec::new());
        };
        let service_day_start = self
            .network
            .service_day_start(self.network.service_day(query.after));
        Ok(self
            .candidates(
                timing,
                &query.board_stop_id,
                service_day_start,
                query.after,
                query.limit,
            )
            .into_iter()
            .filter_map(|(departure_at, timing_basis)| {
                self.effective_trip(
                    &query.service_id,
                    timing,
                    departure_at,
                    timing_basis,
                    &query.board_stop_id,
                )
            })
            .collect())
    }
}

fn expand_frequency(frequency: &MetroFrequency, after_seconds: u32, limit: usize) -> Vec<u32> {
    if frequency.headway_seconds == 0 || frequency.start_seconds > frequency.end_seconds {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut seconds = if after_seconds <= frequency.start_seconds {
        frequency.start_seconds
    } else {
        let elapsed = after_seconds.saturating_sub(frequency.start_seconds);
        let steps =
            elapsed.saturating_add(frequency.headway_seconds - 1) / frequency.headway_seconds;
        frequency
            .start_seconds
            .saturating_add(steps.saturating_mul(frequency.headway_seconds))
    };
    if seconds > frequency.end_seconds {
        return result;
    }
    loop {
        result.push(seconds);
        if result.len() >= limit {
            break;
        }
        let Some(next) = seconds.checked_add(frequency.headway_seconds) else {
            break;
        };
        if next > frequency.end_seconds {
            break;
        }
        seconds = next;
    }
    result
}

fn timing_basis_order(basis: crate::domain::transit_timing::TimingBasis) -> u8 {
    match basis {
        crate::domain::transit_timing::TimingBasis::Schedule => 0,
        crate::domain::transit_timing::TimingBasis::FrequencyEstimate => 1,
        crate::domain::transit_timing::TimingBasis::RealtimePrediction => 2,
        crate::domain::transit_timing::TimingBasis::DelayAdjustedSchedule => 3,
    }
}
