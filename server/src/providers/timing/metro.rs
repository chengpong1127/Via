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
use crate::providers::tdx_bindings::TdxProviderBindings;
use crate::{
    domain::{
        ids::{BoardingPointId, EffectiveTripId, TransitPatternId},
        realtime::{
            ArrivalEstimate, ArrivalState, FreshnessPolicy, FreshnessState, NormalizedRealtimeValue,
        },
        transit::TransitMode,
        transit_runtime::TransitRuntime,
        transit_timing::{
            DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, ScheduleTiming,
            TimingBasis, TimingReliability,
        },
    },
    tdx::TdxClient,
};

#[derive(Clone)]
pub struct MetroDepartureSource {
    network: Arc<TransitRuntime>,
    schedule: ScheduleTiming,
    tdx: Arc<TdxClient>,
    realtime_path: String,
    freshness_policy: FreshnessPolicy,
    batch_size: usize,
    response_limit: usize,
    max_clock_skew_seconds: u32,
    bindings: Arc<TdxProviderBindings>,
}

#[derive(Clone)]
struct MetroRealtimeMatch {
    line_id: String,
    station_id: String,
    destination_station_id: String,
}

impl MetroDepartureSource {
    pub fn new(network: Arc<TransitRuntime>, tdx: Arc<TdxClient>) -> Self {
        Self {
            schedule: ScheduleTiming::new(network.clone()),
            network,
            tdx: tdx.clone(),
            realtime_path: DEFAULT_LIVE_BOARD_PATH.into(),
            freshness_policy: tdx.settings.realtime.freshness_policy(),
            batch_size: tdx.settings.batch.metro_pairs,
            response_limit: tdx.settings.batch.metro_response_limit,
            max_clock_skew_seconds: tdx.settings.realtime.max_clock_skew_seconds,
            bindings: Arc::new(TdxProviderBindings::default()),
        }
    }

    pub fn with_bindings(mut self, bindings: Arc<TdxProviderBindings>) -> Self {
        self.bindings = bindings;
        self
    }

    pub fn with_realtime_endpoint(mut self, path: impl Into<String>) -> Self {
        self.realtime_path = path.into();
        self
    }

    pub fn with_freshness_policy(mut self, freshness_policy: FreshnessPolicy) -> Self {
        self.freshness_policy = freshness_policy;
        self
    }

    fn validate_query(&self, query: &DepartureQuery) -> Result<(), DepartureError> {
        let Some(pattern) = self.network.pattern(&query.pattern_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if self.network.route_of(pattern).mode != TransitMode::Metro
            || !pattern.stops.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        Ok(())
    }

    fn realtime_match(&self, query: &DepartureQuery) -> Option<MetroRealtimeMatch> {
        let pattern = self.network.pattern(&query.pattern_id)?;
        let binding = self.bindings.metro_patterns.get(&query.pattern_id)?;
        if binding.direction > 1 {
            return None;
        }
        let destination_station_id = pattern.stops.last()?;
        Some(MetroRealtimeMatch {
            line_id: binding.line_id.clone(),
            station_id: local_station_id(&query.board_stop_id)?.to_owned(),
            destination_station_id: local_station_id(destination_station_id)?.to_owned(),
        })
    }

    fn effective_trip(
        &self,
        pattern_id: &TransitPatternId,
        departure_at: OffsetDateTime,
        observed_at: OffsetDateTime,
        board_stop_id: &BoardingPointId,
        board_index: usize,
    ) -> Option<EffectiveTrip> {
        let pattern = self.network.pattern(pattern_id)?;
        if pattern.stops.get(board_index) != Some(board_stop_id) {
            return None;
        }
        let mut offsets = vec![0_i64; pattern.stops.len()];
        for index in (board_index + 1)..pattern.stops.len() {
            offsets[index] = offsets[index - 1].checked_add(i64::from(
                pattern.static_profile.segments[index - 1].seconds,
            ))?;
        }
        for index in (0..board_index).rev() {
            offsets[index] = offsets[index + 1]
                .checked_sub(i64::from(pattern.static_profile.segments[index].seconds))?;
        }
        let stop_times = pattern
            .stops
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
            .collect();
        Some(EffectiveTrip {
            id: EffectiveTripId::new(format!(
                "metro:realtime:{}:{}:{}",
                pattern_id.as_str(),
                board_stop_id.as_str(),
                departure_at.unix_timestamp_nanos()
            )),
            pattern_id: pattern_id.clone(),
            scheduled_trip_id: None,
            stop_times,
            timing_basis: TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: Some(observed_at),
            uncertainty_seconds: None,
            evidence: vec![NormalizedRealtimeValue {
                freshness: FreshnessState::Fresh,
                value: ArrivalEstimate {
                    transit_pattern_id: pattern_id.clone(),
                    boarding_point_id: board_stop_id.clone(),
                    predicted_arrival_at: Some(departure_at),
                    state: ArrivalState::Predicted,
                    transmitted_at: observed_at,
                },
            }],
        })
    }

    fn resolve_realtime_query(
        &self,
        query: &DepartureQuery,
        realtime_match: &MetroRealtimeMatch,
        observations: &[MetroObservation],
    ) -> Vec<EffectiveTrip> {
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
                    &query.pattern_id,
                    observation.departure_at,
                    observation.observed_at,
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
        for pairs in request_keys.chunks(self.batch_size) {
            let filter = metro_filter(pairs);
            let response_limit = self.response_limit.to_string();
            let request = [
                ("$filter", filter.as_str()),
                ("$top", response_limit.as_str()),
                ("$format", "JSON"),
            ];
            let payload = self
                .tdx
                .get_realtime_json::<Vec<TdxMetroLiveBoard>>(&self.realtime_path, &request)
                .await;
            let parsed = match payload {
                Ok(payload) if payload.len() < self.response_limit => parse_realtime_payload(
                    payload,
                    &self.freshness_policy,
                    self.max_clock_skew_seconds,
                ),
                Ok(_) => Err(DepartureError::InvalidResponse),
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
        let board_index = query.resolve_board_index(&self.network)?;
        let mut departures = self.schedule.next_departures(query.clone()).await?;
        departures.extend(self.schedule.next_frequency_departures(query.clone())?);
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
        Some(self.realtime_departures(queries).await)
    }
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
