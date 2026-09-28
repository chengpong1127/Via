use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use time::{Duration as TimeDuration, OffsetDateTime};

use super::bus_realtime::{
    DEFAULT_ARRIVAL_PATH, map_tdx_error, odata_in_filter, parse_bus_realtime_payload,
};
use super::metrics::BusTimingMetrics;
use super::source::TransitDepartureSource;
use crate::providers::bindings::TdxProviderBindings;
use crate::{
    domain::{
        ids::{BoardingPointId, EffectiveTripId, TransitPatternId},
        realtime::{
            ArrivalState, FreshnessPolicy, FreshnessState, NormalizedRealtimeValue, RealtimeValue,
        },
        transit::TransitMode,
        transit_runtime::TransitRuntime,
        transit_timing::{
            DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, ScheduleTiming,
            TimingReliability,
        },
    },
    tdx::TdxClient,
};

const MAX_REALTIME_STOPS_PER_REQUEST: usize = 100;

pub struct BusDepartureSource {
    network: Arc<TransitRuntime>,
    tdx: Option<Arc<TdxClient>>,
    schedule: ScheduleTiming,
    realtime_path: String,
    freshness_policy: FreshnessPolicy,
    metrics: BusTimingMetrics,
    bindings: Arc<TdxProviderBindings>,
}

impl BusDepartureSource {
    pub fn new(network: Arc<TransitRuntime>, tdx: Option<Arc<TdxClient>>) -> Self {
        Self {
            schedule: ScheduleTiming::new(network.clone()),
            network,
            tdx,
            realtime_path: DEFAULT_ARRIVAL_PATH.into(),
            freshness_policy: FreshnessPolicy::tdx_one_minute("1"),
            metrics: BusTimingMetrics::default(),
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

    pub fn with_metrics(mut self, metrics: BusTimingMetrics) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn with_freshness_policy(mut self, freshness_policy: FreshnessPolicy) -> Self {
        self.freshness_policy = freshness_policy;
        self
    }

    fn effective_trip(
        &self,
        query: &DepartureQuery,
        departure_at: OffsetDateTime,
        observed_at: OffsetDateTime,
        evidence: NormalizedRealtimeValue,
    ) -> Option<EffectiveTrip> {
        let pattern = self.network.pattern(&query.pattern_id)?;
        let board_index = query.resolve_board_index(&self.network).ok()?;
        let mut stop_times = Vec::<EffectiveStopTime>::with_capacity(pattern.stops.len());
        let mut previous_departure = None;
        for (index, stop_id) in pattern.stops.iter().enumerate() {
            let stop_time = if index <= board_index {
                EffectiveStopTime {
                    stop_id: stop_id.clone(),
                    arrival_at: departure_at,
                    departure_at,
                }
            } else {
                let duration = pattern.static_profile.segments.get(index - 1)?.seconds;
                let previous = previous_departure
                    .or_else(|| stop_times.last().map(|time| time.departure_at))?;
                let arrival_at = previous + TimeDuration::seconds(i64::from(duration));
                EffectiveStopTime {
                    stop_id: stop_id.clone(),
                    arrival_at,
                    departure_at: arrival_at,
                }
            };
            if stop_time.arrival_at > stop_time.departure_at
                || previous_departure.is_some_and(|previous| previous > stop_time.arrival_at)
            {
                return None;
            }
            previous_departure = Some(stop_time.departure_at);
            stop_times.push(stop_time);
        }
        Some(EffectiveTrip {
            id: EffectiveTripId::new(format!(
                "runtime:{}:{}:{}",
                query.pattern_id.as_str(),
                query.board_stop_id.as_str(),
                departure_at.unix_timestamp_nanos()
            )),
            pattern_id: query.pattern_id.clone(),
            scheduled_trip_id: None,
            stop_times,
            timing_basis: crate::domain::transit_timing::TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: Some(observed_at),
            uncertainty_seconds: None,
            evidence: vec![evidence],
        })
    }

    fn resolve_query(
        &self,
        query: &DepartureQuery,
        values: &[NormalizedRealtimeValue],
    ) -> Vec<EffectiveTrip> {
        let mut departures = values
            .iter()
            .filter_map(|status| {
                if status.freshness.state != FreshnessState::Fresh {
                    return None;
                }
                let RealtimeValue::ArrivalEstimate(arrival) = &status.value else {
                    return None;
                };
                if arrival.transit_pattern_id != query.pattern_id
                    || arrival.boarding_point_id != query.board_stop_id
                    || !matches!(
                        arrival.state,
                        ArrivalState::Predicted | ArrivalState::AtBoardingPoint
                    )
                {
                    return None;
                }
                let departure_at = arrival.predicted_arrival_at?;
                if departure_at < query.after
                    || query.before.is_some_and(|before| departure_at > before)
                {
                    return None;
                }
                self.effective_trip(query, departure_at, arrival.transmitted_at, status.clone())
            })
            .collect::<Vec<_>>();
        departures.sort_by(|left, right| {
            left.stop_times
                .iter()
                .find(|stop_time| stop_time.stop_id == query.board_stop_id)
                .map(|stop_time| stop_time.departure_at)
                .cmp(
                    &right
                        .stop_times
                        .iter()
                        .find(|stop_time| stop_time.stop_id == query.board_stop_id)
                        .map(|stop_time| stop_time.departure_at),
                )
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        departures.truncate(query.limit);
        departures
    }

    async fn realtime_departures(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
        let started_at = Instant::now();
        if queries.is_empty() {
            return Vec::new();
        }

        let active_queries = queries
            .iter()
            .filter(|query| query.limit > 0)
            .collect::<Vec<_>>();
        if active_queries.is_empty() {
            return queries.into_iter().map(|_| Ok(Vec::new())).collect();
        }

        let mut stops_by_scope = BTreeMap::<String, HashSet<BoardingPointId>>::new();
        let mut services_by_scope = BTreeMap::<String, HashSet<TransitPatternId>>::new();
        let mut pairs_by_scope =
            BTreeMap::<String, HashSet<(TransitPatternId, BoardingPointId)>>::new();
        let mut unscoped_queries = 0;
        for query in &active_queries {
            let pair = (query.pattern_id.clone(), query.board_stop_id.clone());
            let Some(pattern) = self.network.pattern(&query.pattern_id) else {
                unscoped_queries += 1;
                continue;
            };
            let scope = self
                .bindings
                .bus_patterns
                .get(&pattern.id)
                .map(|binding| binding.realtime_scope.clone())
                .or_else(|| (self.realtime_path != DEFAULT_ARRIVAL_PATH).then(String::new));
            let Some(scope) = scope else {
                unscoped_queries += 1;
                continue;
            };
            stops_by_scope
                .entry(scope.clone())
                .or_default()
                .insert(query.board_stop_id.clone());
            services_by_scope
                .entry(scope.clone())
                .or_default()
                .insert(query.pattern_id.clone());
            pairs_by_scope.entry(scope).or_default().insert(pair);
        }
        let relevant_stop_count = stops_by_scope.values().map(HashSet::len).sum();
        let batch_count = stops_by_scope
            .values()
            .map(|stops| stops.len().div_ceil(MAX_REALTIME_STOPS_PER_REQUEST))
            .sum();
        self.metrics
            .record_departure_batch(active_queries.len(), relevant_stop_count, batch_count);

        let mut values_by_pair =
            HashMap::<(TransitPatternId, BoardingPointId), Vec<NormalizedRealtimeValue>>::new();
        let mut errors_by_pair =
            HashMap::<(TransitPatternId, BoardingPointId), DepartureError>::new();
        let mut http_requests = 0usize;
        let mut rows_received = 0usize;
        let mut rows_relevant = 0usize;
        let scope_count = stops_by_scope.len();
        for (scope, unique_stops) in stops_by_scope {
            let mut unique_stops = unique_stops.into_iter().collect::<Vec<_>>();
            unique_stops.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            let mut requested_services = HashMap::<String, Vec<TransitPatternId>>::new();
            for pattern_id in services_by_scope.get(&scope).into_iter().flatten() {
                if let Some(binding) = self.bindings.bus_patterns.get(pattern_id) {
                    requested_services
                        .entry(binding.tdx_service_id.clone())
                        .or_default()
                        .push(pattern_id.clone());
                }
            }
            let scope_pairs = pairs_by_scope.get(&scope).cloned().unwrap_or_default();
            let mut completed_stops = HashSet::new();
            let mut rate_limited = false;
            for stops in unique_stops.chunks(MAX_REALTIME_STOPS_PER_REQUEST) {
                if rate_limited {
                    for pair in scope_pairs.iter().filter(|(_, stop)| stops.contains(stop)) {
                        errors_by_pair.insert(pair.clone(), DepartureError::RateLimited);
                    }
                    continue;
                }
                let requested_stops = stops.iter().cloned().collect::<HashSet<_>>();
                let path = match self.realtime_path_for_scope(Some(&scope)) {
                    Ok(path) => path,
                    Err(error) => {
                        for pair in scope_pairs.iter().filter(|(_, stop)| stops.contains(stop)) {
                            errors_by_pair.insert(pair.clone(), error.clone());
                        }
                        continue;
                    }
                };
                let boarding_point_ids = stops
                    .iter()
                    .map(|stop| stop.as_str().to_owned())
                    .collect::<Vec<_>>();
                let filter = odata_in_filter("StopUID", &boarding_point_ids);
                let query = [("$filter", filter.as_str())];
                self.metrics.record_departure_upstream_request();
                http_requests += 1;
                let payload = self
                    .tdx
                    .as_ref()
                    .expect("realtime_departures requires a TDX client")
                    .get_json::<serde_json::Value>(&path, &query)
                    .await;
                let values = match payload {
                    Ok(payload) => parse_bus_realtime_payload(
                        &payload,
                        &requested_services,
                        &self.freshness_policy,
                    ),
                    Err(error) => Err(map_tdx_error(error)),
                };
                self.metrics
                    .record_departure_upstream_result(values.as_ref().err());
                let values = match values {
                    Ok(values) => {
                        completed_stops.extend(requested_stops.iter().cloned());
                        rows_received += values.len();
                        values
                    }
                    Err(error) => {
                        if matches!(error, DepartureError::RateLimited) {
                            rate_limited = true;
                        }
                        for pair in scope_pairs.iter().filter(|(_, stop)| stops.contains(stop)) {
                            errors_by_pair.insert(pair.clone(), error.clone());
                        }
                        continue;
                    }
                };
                for value in values {
                    let RealtimeValue::ArrivalEstimate(arrival) = &value.value else {
                        continue;
                    };
                    let pair = (
                        arrival.transit_pattern_id.clone(),
                        arrival.boarding_point_id.clone(),
                    );
                    if requested_stops.contains(&arrival.boarding_point_id)
                        && scope_pairs.contains(&pair)
                    {
                        rows_relevant += 1;
                        values_by_pair.entry(pair).or_default().push(value);
                    }
                }
            }
            if rate_limited {
                for pair in scope_pairs
                    .into_iter()
                    .filter(|(_, stop)| !completed_stops.contains(stop))
                {
                    errors_by_pair
                        .entry(pair)
                        .or_insert(DepartureError::RateLimited);
                }
            }
        }

        let outcome = if errors_by_pair
            .values()
            .any(|error| matches!(error, DepartureError::RateLimited))
        {
            "rate_limited"
        } else if errors_by_pair.is_empty() {
            "success"
        } else {
            "fallback"
        };
        self.metrics
            .record_snapshot_rows(rows_received, rows_relevant);
        tracing::debug!(
            target: "via_server::providers::timing",
            relevant_stops = relevant_stop_count,
            scopes = scope_count,
            http_requests,
            rows_received,
            rows_relevant,
            unscoped_queries,
            outcome,
            latency_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "bus timing snapshot"
        );

        queries
            .into_iter()
            .map(|query| {
                if query.limit == 0 {
                    Ok(Vec::new())
                } else {
                    let pair = (query.pattern_id.clone(), query.board_stop_id.clone());
                    errors_by_pair.get(&pair).cloned().map_or_else(
                        || {
                            Ok(self.resolve_query(
                                &query,
                                values_by_pair.get(&pair).map_or(&[], Vec::as_slice),
                            ))
                        },
                        Err,
                    )
                }
            })
            .collect()
    }

    fn realtime_path_for_scope(&self, scope: Option<&str>) -> Result<String, DepartureError> {
        if self.realtime_path != DEFAULT_ARRIVAL_PATH {
            return Ok(self.realtime_path.clone());
        }
        let scope = scope.ok_or(DepartureError::InvalidConfiguration)?;
        if scope.is_empty()
            || !scope
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            return Err(DepartureError::InvalidConfiguration);
        }
        Ok(format!(
            "{}/{scope}",
            self.realtime_path.trim_end_matches('/')
        ))
    }
}

#[async_trait]
impl TransitDepartureSource for BusDepartureSource {
    fn mode(&self) -> TransitMode {
        TransitMode::Bus
    }

    fn static_timing_reliability(&self, _query: &DepartureQuery) -> TimingReliability {
        TimingReliability::Fallback
    }

    async fn static_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        let Some(pattern) = self.network.pattern(&query.pattern_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if self.network.route_of(pattern).mode != TransitMode::Bus
            || !pattern.stops.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        let mut departures = self.schedule.next_departures(query.clone()).await?;
        departures.extend(self.schedule.next_frequency_departures(query.clone())?);
        let board_index = query.resolve_board_index(&self.network)?;
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

    async fn realtime_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Option<Vec<Result<Vec<EffectiveTrip>, DepartureError>>> {
        self.tdx.as_ref()?;
        Some(self.realtime_departures(queries).await)
    }
}
