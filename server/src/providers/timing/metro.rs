use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use time::{Duration as TimeDuration, OffsetDateTime};

use super::{
    live_error::departure_error,
    metro_realtime::{MetroObservation, parse_metro_boards},
    source::TransitDepartureSource,
};
use crate::providers::tdx_bindings::TdxProviderBindings;
use crate::{
    domain::{
        ids::{BoardingPointId, EffectiveTripId, TransitPatternId},
        realtime::{
            ArrivalEstimate, ArrivalState, FreshnessPolicy, FreshnessState, NormalizedRealtimeValue,
        },
        transit::network::TransitMode,
        transit::runtime::TransitRuntime,
        transit::timing::{
            DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, ScheduleTiming,
            TimingBasis, TimingReliability,
        },
    },
    live::{CachedLiveData, DirectLiveData, LiveDataSource, MetroPair},
    tdx::{RealtimeSettings, TdxClient},
};

#[derive(Clone)]
pub struct MetroDepartureSource {
    transit: Arc<TransitRuntime>,
    schedule: ScheduleTiming,
    live: Arc<dyn LiveDataSource>,
    freshness_policy: FreshnessPolicy,
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
    pub fn new(transit: Arc<TransitRuntime>, tdx: Arc<TdxClient>) -> Self {
        let settings = tdx.settings.clone();
        Self::with_live_data(
            transit,
            Arc::new(CachedLiveData::new(
                Arc::new(DirectLiveData::new(tdx)),
                (&settings).into(),
            )),
            &settings.realtime,
        )
    }

    pub fn with_live_data(
        transit: Arc<TransitRuntime>,
        live: Arc<dyn LiveDataSource>,
        realtime: &RealtimeSettings,
    ) -> Self {
        Self {
            schedule: ScheduleTiming::new(transit.clone()),
            transit,
            live,
            freshness_policy: realtime.freshness_policy(),
            max_clock_skew_seconds: realtime.max_clock_skew_seconds,
            bindings: Arc::new(TdxProviderBindings::default()),
        }
    }

    pub fn with_bindings(mut self, bindings: Arc<TdxProviderBindings>) -> Self {
        self.bindings = bindings;
        self
    }

    pub fn with_freshness_policy(mut self, freshness_policy: FreshnessPolicy) -> Self {
        self.freshness_policy = freshness_policy;
        self
    }

    fn validate_query(&self, query: &DepartureQuery) -> Result<(), DepartureError> {
        let Some(pattern) = self.transit.pattern(&query.pattern_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if self.transit.required_route(&pattern.route_id).mode != TransitMode::Metro
            || !pattern.stops.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        Ok(())
    }

    fn realtime_match(&self, query: &DepartureQuery) -> Option<MetroRealtimeMatch> {
        let pattern = self.transit.pattern(&query.pattern_id)?;
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
        let pattern = self.transit.pattern(pattern_id)?;
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
        let Ok(board_index) = query.resolve_board_index(&self.transit) else {
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
        let mut query_indices_by_pair = BTreeMap::<MetroPair, Vec<usize>>::new();
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
            query_indices_by_pair
                .entry(MetroPair {
                    line_id: realtime_match.line_id.clone(),
                    station_id: realtime_match.station_id.clone(),
                })
                .or_default()
                .push(index);
            matches[index] = Some(realtime_match);
        }

        if query_indices_by_pair.is_empty() {
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

        let request_keys = query_indices_by_pair.keys().cloned().collect::<Vec<_>>();
        let batch = self.live.metro_boards(&request_keys).await;
        let mut errors_by_query = HashMap::new();
        for unavailable in &batch.unavailable {
            for index in query_indices_by_pair
                .get(&unavailable.id)
                .into_iter()
                .flatten()
            {
                errors_by_query.insert(*index, departure_error(unavailable.reason));
            }
        }
        let observations = match parse_metro_boards(
            batch.records,
            &self.freshness_policy,
            self.max_clock_skew_seconds,
        ) {
            Ok(observations) => observations,
            Err(error) => {
                for index in query_indices_by_pair.values().flatten() {
                    errors_by_query
                        .entry(*index)
                        .or_insert_with(|| error.clone());
                }
                Vec::new()
            }
        };

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
        let board_index = query.resolve_board_index(&self.transit)?;
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
