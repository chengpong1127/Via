use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use time::{Duration as TimeDuration, OffsetDateTime};

use super::{
    metro_realtime::{
        DEFAULT_LIVE_BOARD_PATH, MetroObservation, TdxMetroLiveBoard, map_tdx_error,
        parse_realtime_payload,
    },
    source::TransitDepartureSource,
};
use crate::{
    domain::{
        ids::{BoardingPointId, EffectiveTripId, TransitServiceId},
        realtime::FreshnessPolicy,
        transit::{MetroFrequency, MetroTiming, TransitMode, TransitNetwork},
        transit_timing::{
            DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, ScheduleTiming,
            TimingBasis, TimingReliability,
        },
    },
    tdx::TdxClient,
};

const MAX_REALTIME_BATCH_PAIRS: usize = 64;

/// Materializes Metro departures from station-level schedules or headways.
///
/// GTFS schedules and frequency templates provide static Metro timing.
/// Realtime LiveBoard observations only replace the boarding-station
/// departure; the same static travel-time shape is retained for downstream
/// stops.
#[derive(Clone)]
pub struct MetroDepartureSource {
    network: Arc<TransitNetwork>,
    schedule: ScheduleTiming,
    tdx: Option<Arc<TdxClient>>,
    realtime_path: String,
    freshness_policy: FreshnessPolicy,
}

#[derive(Clone)]
struct MetroRealtimeMatch {
    line_id: String,
    station_id: String,
    destination_station_id: String,
}

#[derive(Clone, Copy)]
struct MetroTimingMetadata {
    basis: TimingBasis,
    reliability: TimingReliability,
    observed_at: Option<OffsetDateTime>,
}

impl MetroDepartureSource {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        Self {
            schedule: ScheduleTiming::new(network.clone()),
            network,
            tdx: None,
            realtime_path: DEFAULT_LIVE_BOARD_PATH.into(),
            freshness_policy: FreshnessPolicy::tdx_one_minute("metro_live_board_v1"),
        }
    }

    pub fn with_tdx(mut self, tdx: Option<Arc<TdxClient>>) -> Self {
        self.tdx = tdx;
        self
    }

    /// Override the Metro endpoint for a fixture or a TDX deployment variant.
    pub fn with_realtime_endpoint(mut self, path: impl Into<String>) -> Self {
        self.realtime_path = path.into();
        self
    }

    pub fn with_freshness_policy(mut self, freshness_policy: FreshnessPolicy) -> Self {
        self.freshness_policy = freshness_policy;
        self
    }

    fn validate_query(&self, query: &DepartureQuery) -> Result<(), DepartureError> {
        let Some(service) = self.network.service(&query.service_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if service.transit_mode != TransitMode::Metro
            || !service.pattern.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        Ok(())
    }

    fn realtime_match(&self, query: &DepartureQuery) -> Option<MetroRealtimeMatch> {
        let service = self.network.service(&query.service_id)?;
        let (line_id, direction) = metro_service_parts(&query.service_id)?;
        if direction > 1 {
            return None;
        }
        let destination_station_id = service.pattern.last()?;
        Some(MetroRealtimeMatch {
            line_id: line_id.to_owned(),
            station_id: local_station_id(&query.board_stop_id)?.to_owned(),
            destination_station_id: local_station_id(destination_station_id)?.to_owned(),
        })
    }

    fn candidates(
        &self,
        timing: &MetroTiming,
        board_stop_id: &BoardingPointId,
        service_day_start: OffsetDateTime,
        after: OffsetDateTime,
        before: Option<OffsetDateTime>,
        limit: usize,
    ) -> Vec<(OffsetDateTime, TimingBasis)> {
        let Some(station_timing) = timing
            .station_timings
            .iter()
            .find(|station| &station.stop_id == board_stop_id)
        else {
            return Vec::new();
        };
        let after_seconds =
            u32::try_from((after - service_day_start).whole_seconds().max(0)).unwrap_or(u32::MAX);
        let before_seconds = before
            .map(|before| {
                u32::try_from((before - service_day_start).whole_seconds().max(0))
                    .unwrap_or(u32::MAX)
            })
            .unwrap_or(u32::MAX);
        let mut candidates = station_timing
            .departures_seconds
            .iter()
            .copied()
            .map(|seconds| {
                (
                    service_day_start + TimeDuration::seconds(i64::from(seconds)),
                    TimingBasis::Schedule,
                )
            })
            .chain(
                station_timing
                    .frequencies
                    .iter()
                    .flat_map(|frequency| {
                        expand_frequency(frequency, after_seconds, before_seconds, limit)
                    })
                    .map(|seconds| {
                        (
                            service_day_start + TimeDuration::seconds(i64::from(seconds)),
                            TimingBasis::FrequencyEstimate,
                        )
                    }),
            )
            .filter(|(departure_at, _)| {
                *departure_at >= after && before.is_none_or(|before| *departure_at <= before)
            })
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
        service_id: &TransitServiceId,
        timing: &MetroTiming,
        departure_at: OffsetDateTime,
        metadata: MetroTimingMetadata,
        board_stop_id: &BoardingPointId,
        board_index: usize,
    ) -> Option<EffectiveTrip> {
        let service = self.network.service(service_id)?;
        if service.pattern.get(board_index) != Some(board_stop_id) {
            return None;
        }
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
                let at = departure_at + TimeDuration::seconds(offset);
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
        let id_prefix = if metadata.basis == TimingBasis::RealtimePrediction {
            "metro:realtime"
        } else {
            "metro"
        };
        Some(EffectiveTrip {
            id: EffectiveTripId::new(format!(
                "{id_prefix}:{}:{}:{}",
                service_id.as_str(),
                board_stop_id.as_str(),
                departure_at.unix_timestamp_nanos()
            )),
            service_id: service_id.clone(),
            scheduled_trip_id: None,
            stop_times,
            timing_basis: metadata.basis,
            timing_reliability: metadata.reliability,
            observed_at: metadata.observed_at,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        })
    }

    fn resolve_realtime_query(
        &self,
        query: &DepartureQuery,
        realtime_match: &MetroRealtimeMatch,
        observations: &[MetroObservation],
    ) -> Vec<EffectiveTrip> {
        let Some(timing) = self
            .network
            .metro_timings
            .iter()
            .find(|timing| timing.service_id == query.service_id)
        else {
            return Vec::new();
        };
        let Ok(board_index) = query.resolve_board_index(&self.network) else {
            return Vec::new();
        };
        let mut departures = observations
            .iter()
            .filter(|observation| {
                observation.line_id == realtime_match.line_id
                    && observation.station_id == realtime_match.station_id
                    && observation.destination_station_id == realtime_match.destination_station_id
                    && observation.departure_at >= query.after
                    && query
                        .before
                        .is_none_or(|before| observation.departure_at <= before)
            })
            .filter_map(|observation| {
                self.effective_trip(
                    &query.service_id,
                    timing,
                    observation.departure_at,
                    MetroTimingMetadata {
                        basis: TimingBasis::RealtimePrediction,
                        reliability: TimingReliability::Reliable,
                        observed_at: Some(observation.observed_at),
                    },
                    &query.board_stop_id,
                    board_index,
                )
            })
            .collect::<Vec<_>>();
        departures.sort_by(|left, right| {
            left.stop_times[board_index]
                .departure_at
                .cmp(&right.stop_times[board_index].departure_at)
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        departures.dedup_by(|left, right| {
            left.stop_times[board_index].departure_at == right.stop_times[board_index].departure_at
        });
        departures.truncate(query.limit);
        departures
    }

    async fn realtime_departures(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
        let mut requested_pairs = BTreeMap::<(String, String), Vec<usize>>::new();
        let mut matches = vec![None; queries.len()];
        let mut invalid_queries = HashSet::new();
        for (index, query) in queries.iter().enumerate() {
            if query.limit == 0 {
                continue;
            }
            if self.validate_query(query).is_err() {
                invalid_queries.insert(index);
                continue;
            }
            let Some(realtime_match) = self.realtime_match(query) else {
                continue;
            };
            requested_pairs
                .entry((
                    realtime_match.line_id.clone(),
                    realtime_match.station_id.clone(),
                ))
                .or_default()
                .push(index);
            matches[index] = Some(realtime_match);
        }

        if requested_pairs.is_empty() {
            return queries
                .into_iter()
                .enumerate()
                .map(|(index, _query)| {
                    if invalid_queries.contains(&index) {
                        Err(DepartureError::InvalidQuery)
                    } else {
                        Ok(Vec::new())
                    }
                })
                .collect();
        }

        let request_keys = requested_pairs.keys().cloned().collect::<Vec<_>>();
        let mut observations = Vec::new();
        let mut errors_by_query = HashMap::new();
        for pairs in request_keys.chunks(MAX_REALTIME_BATCH_PAIRS) {
            let filter = metro_filter(pairs);
            let request = [
                ("$filter", filter.as_str()),
                ("$top", "1000"),
                ("$format", "JSON"),
            ];
            let payload = self
                .tdx
                .as_ref()
                .expect("realtime_departures requires a TDX client")
                .get_json::<Vec<TdxMetroLiveBoard>>(&self.realtime_path, &request)
                .await;
            let parsed = match payload {
                Ok(payload) => parse_realtime_payload(payload, &self.freshness_policy),
                Err(error) => Err(map_tdx_error(error)),
            };
            match parsed {
                Ok(values) => observations.extend(values),
                Err(error) => {
                    for pair in pairs {
                        if let Some(indexes) = requested_pairs.get(pair) {
                            for index in indexes {
                                errors_by_query.insert(*index, error.clone());
                            }
                        }
                    }
                }
            }
        }

        queries
            .into_iter()
            .enumerate()
            .map(|(index, query)| {
                if invalid_queries.contains(&index) {
                    Err(DepartureError::InvalidQuery)
                } else if let Some(error) = errors_by_query.get(&index).cloned() {
                    Err(error)
                } else if query.limit == 0 {
                    Ok(Vec::new())
                } else {
                    Ok(matches[index]
                        .as_ref()
                        .map_or_else(Vec::new, |realtime_match| {
                            self.resolve_realtime_query(&query, realtime_match, &observations)
                        }))
                }
            })
            .collect()
    }

    async fn static_departures_for_query(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        self.validate_query(&query)?;
        self.validate_query(&query)?;
        let timing = self
            .network
            .metro_timings
            .iter()
            .find(|timing| timing.service_id == query.service_id);
        let board_index = query.resolve_board_index(&self.network)?;
        let mut departures = self.schedule.next_departures(query.clone()).await?;
        departures.extend(self.schedule.next_frequency_departures(query.clone())?);
        let Some(timing) = timing else {
            return Ok(departures);
        };
        let first_service_day = self.network.service_day(query.after);
        let last_service_day = query
            .before
            .map(|before| self.network.service_day(before))
            .unwrap_or(first_service_day);
        let mut service_day = first_service_day;
        let mut candidates = Vec::new();
        loop {
            candidates.extend(self.candidates(
                timing,
                &query.board_stop_id,
                self.network.service_day_start(service_day),
                query.after,
                query.before,
                query.limit,
            ));
            if service_day >= last_service_day {
                break;
            }
            let Some(next_day) = service_day.next_day() else {
                break;
            };
            service_day = next_day;
        }
        candidates.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(timing_basis_order(left.1).cmp(&timing_basis_order(right.1)))
        });
        candidates.dedup_by(|left, right| left.0 == right.0);
        candidates.truncate(query.limit);
        departures.extend(
            candidates
                .into_iter()
                .filter_map(|(departure_at, timing_basis)| {
                    self.effective_trip(
                        &query.service_id,
                        timing,
                        departure_at,
                        MetroTimingMetadata {
                            basis: timing_basis,
                            reliability: TimingReliability::Fallback,
                            observed_at: None,
                        },
                        &query.board_stop_id,
                        board_index,
                    )
                }),
        );
        departures.retain(|trip| {
            let departure = trip.stop_times[board_index].departure_at;
            departure >= query.after && query.before.is_none_or(|before| departure <= before)
        });
        departures.sort_by(|left, right| {
            left.stop_times[board_index]
                .departure_at
                .cmp(&right.stop_times[board_index].departure_at)
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        departures.dedup_by(|left, right| {
            left.stop_times[board_index].departure_at == right.stop_times[board_index].departure_at
        });
        departures.truncate(query.limit);
        Ok(departures)
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
        self.static_departures_for_query(query).await
    }

    async fn realtime_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Option<Vec<Result<Vec<EffectiveTrip>, DepartureError>>> {
        self.tdx.as_ref()?;
        Some(self.realtime_departures(queries).await)
    }
}

fn metro_service_parts(service_id: &TransitServiceId) -> Option<(&str, u8)> {
    let service = service_id.as_str().strip_prefix("metro:")?;
    let (line_id, direction) = service.rsplit_once("::direction=")?;
    if line_id.is_empty() {
        return None;
    }
    Some((line_id, direction.split("::").next()?.parse().ok()?))
}

fn local_station_id(stop_id: &BoardingPointId) -> Option<&str> {
    let station_id = stop_id.as_str();
    let local = station_id
        .strip_prefix("TRTC-")
        .unwrap_or(station_id)
        .trim();
    (!local.is_empty()).then_some(local)
}

fn metro_filter(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(line_id, station_id)| {
            format!(
                "(LineID eq '{}' and StationID eq '{}')",
                escape_odata_literal(line_id),
                escape_odata_literal(station_id)
            )
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

fn escape_odata_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn expand_frequency(
    frequency: &MetroFrequency,
    after_seconds: u32,
    before_seconds: u32,
    limit: usize,
) -> Vec<u32> {
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
    let end_seconds = frequency.end_seconds.min(before_seconds);
    if seconds > end_seconds {
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
        if next > end_seconds {
            break;
        }
        seconds = next;
    }
    result
}

fn timing_basis_order(basis: TimingBasis) -> u8 {
    match basis {
        TimingBasis::Schedule => 0,
        TimingBasis::FrequencyEstimate => 1,
        TimingBasis::RealtimePrediction => 2,
        TimingBasis::DelayAdjustedSchedule => 3,
    }
}
