use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Instant,
};

use async_trait::async_trait;
use time::{Duration as TimeDuration, OffsetDateTime};

use super::bus_realtime::parse_bus_arrivals;
use super::context::TdxTimingContext;
use super::live_error::departure_error;
use super::source::TransitDepartureSource;
use crate::{
    domain::{
        ids::{BoardingPointId, EffectiveTripId, TransitPatternId},
        realtime::{ArrivalState, FreshnessState, NormalizedRealtimeValue},
        transit::network::TransitMode,
        transit::service::ServicePeriod,
        transit::timing::{
            DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, TimingBasis,
            TimingReliability,
        },
    },
    live::UnavailableReason,
};

struct RealtimeRequestPlan {
    stops_by_scope: BTreeMap<String, HashSet<BoardingPointId>>,
    services_by_scope: BTreeMap<String, HashSet<TransitPatternId>>,
    pairs_by_scope: BTreeMap<String, HashSet<(TransitPatternId, BoardingPointId)>>,
    unscoped_queries: usize,
}

#[derive(Default)]
struct RealtimeDepartureResults {
    values_by_pair: HashMap<(TransitPatternId, BoardingPointId), Vec<NormalizedRealtimeValue>>,
    errors_by_pair: HashMap<(TransitPatternId, BoardingPointId), DepartureError>,
    http_requests: usize,
    rows_received: usize,
    rows_relevant: usize,
}

pub struct BusDepartureSource {
    context: TdxTimingContext,
}

const ESTIMATE_HORIZON_SECONDS: i64 = 2 * 60 * 60;

pub static HEADWAY_ESTIMATES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

impl BusDepartureSource {
    pub fn new(context: TdxTimingContext) -> Self {
        Self { context }
    }

    fn effective_trip(
        &self,
        query: &DepartureQuery,
        departure_at: OffsetDateTime,
        evidence: Option<NormalizedRealtimeValue>,
    ) -> Option<EffectiveTrip> {
        let estimated = evidence.is_none();
        let pattern = self.context.transit.pattern(&query.pattern_id)?;
        let board_index = query.resolve_board_index(&self.context.transit).ok()?;
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
                "{}:{}:{}:{}",
                if estimated { "estimate" } else { "runtime" },
                query.pattern_id.as_str(),
                query.board_stop_id.as_str(),
                departure_at.unix_timestamp_nanos()
            )),
            pattern_id: query.pattern_id.clone(),
            scheduled_trip_id: None,
            stop_times,
            timing_basis: if estimated {
                TimingBasis::FrequencyEstimate
            } else {
                TimingBasis::RealtimePrediction
            },
            timing_reliability: if estimated {
                TimingReliability::Fallback
            } else {
                TimingReliability::Reliable
            },
            observed_at: evidence.as_ref().map(|value| value.value.transmitted_at),
            evidence: evidence.into_iter().collect(),
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
                if status.freshness != FreshnessState::Fresh {
                    return None;
                }
                let arrival = &status.value;
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
                self.effective_trip(query, departure_at, Some(status.clone()))
            })
            .collect::<Vec<_>>();
        if HEADWAY_ESTIMATES.load(std::sync::atomic::Ordering::Relaxed) {
            departures.extend(self.estimated_departures(query, values));
        }
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

    fn estimated_departures(
        &self,
        query: &DepartureQuery,
        values: &[NormalizedRealtimeValue],
    ) -> Vec<EffectiveTrip> {
        let Some(headways) = self
            .context
            .bindings
            .bus_patterns
            .get(&query.pattern_id)
            .map(|binding| &binding.headway_seconds_by_period)
            .filter(|headways| !headways.is_empty())
        else {
            return Vec::new();
        };
        let Some(anchor) = values
            .iter()
            .filter(|status| {
                status.freshness == FreshnessState::Fresh
                    && status.value.transit_pattern_id == query.pattern_id
                    && status.value.boarding_point_id == query.board_stop_id
                    && matches!(
                        status.value.state,
                        ArrivalState::Predicted | ArrivalState::AtBoardingPoint
                    )
            })
            .filter_map(|status| status.value.predicted_arrival_at)
            .max()
        else {
            return Vec::new();
        };
        let horizon = anchor + TimeDuration::seconds(ESTIMATE_HORIZON_SECONDS);
        let mut estimates = Vec::new();
        let mut departure_at = anchor;
        while estimates.len() < query.limit {
            let Some(Some(headway)) = headways
                .get(ServicePeriod::at(departure_at).index())
                .copied()
            else {
                break;
            };
            departure_at += TimeDuration::seconds(i64::from(headway.max(60)));
            if departure_at > horizon || query.before.is_some_and(|before| departure_at > before) {
                break;
            }
            if departure_at < query.after {
                continue;
            }
            if let Some(trip) = self.effective_trip(query, departure_at, None) {
                estimates.push(trip);
            }
        }
        estimates
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

        let plan = self.build_realtime_request_plan(&active_queries);
        let relevant_stop_count: usize = plan.stops_by_scope.values().map(HashSet::len).sum();
        ::metrics::counter!("via_bus_timing_logical_departure_needs_total")
            .increment(active_queries.len() as u64);
        ::metrics::counter!("via_bus_timing_realtime_stops_requested_total")
            .increment(relevant_stop_count as u64);
        ::metrics::counter!("via_bus_timing_realtime_batches_total")
            .increment(plan.stops_by_scope.len() as u64);

        let mut results = RealtimeDepartureResults::default();
        let scope_count = plan.stops_by_scope.len();
        for (scope, unique_stops) in &plan.stops_by_scope {
            self.fetch_scope(scope, unique_stops, &plan, &mut results)
                .await;
        }

        let outcome = if results
            .errors_by_pair
            .values()
            .any(|error| matches!(error, DepartureError::RateLimited))
        {
            "rate_limited"
        } else if results.errors_by_pair.is_empty() {
            "success"
        } else {
            "fallback"
        };
        ::metrics::counter!("via_bus_timing_realtime_rows_received_total")
            .increment(results.rows_received as u64);
        ::metrics::counter!("via_bus_timing_realtime_rows_relevant_total")
            .increment(results.rows_relevant as u64);
        tracing::debug!(
            target: "via_server::providers::timing",
            relevant_stops = relevant_stop_count,
            scopes = scope_count,
            http_requests = results.http_requests,
            rows_received = results.rows_received,
            rows_relevant = results.rows_relevant,
            unscoped_queries = plan.unscoped_queries,
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
                    results.errors_by_pair.get(&pair).cloned().map_or_else(
                        || {
                            Ok(self.resolve_query(
                                &query,
                                results.values_by_pair.get(&pair).map_or(&[], Vec::as_slice),
                            ))
                        },
                        Err,
                    )
                }
            })
            .collect()
    }

    fn build_realtime_request_plan(
        &self,
        active_queries: &[&DepartureQuery],
    ) -> RealtimeRequestPlan {
        let mut stops_by_scope = BTreeMap::<String, HashSet<BoardingPointId>>::new();
        let mut services_by_scope = BTreeMap::<String, HashSet<TransitPatternId>>::new();
        let mut pairs_by_scope =
            BTreeMap::<String, HashSet<(TransitPatternId, BoardingPointId)>>::new();
        let mut unscoped_queries = 0;
        for query in active_queries {
            let pair = (query.pattern_id.clone(), query.board_stop_id.clone());
            let Some(pattern) = self.context.transit.pattern(&query.pattern_id) else {
                unscoped_queries += 1;
                continue;
            };
            let scope = self
                .context
                .bindings
                .bus_patterns
                .get(&pattern.id)
                .map(|binding| binding.realtime_scope.clone());
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
        RealtimeRequestPlan {
            stops_by_scope,
            services_by_scope,
            pairs_by_scope,
            unscoped_queries,
        }
    }

    async fn fetch_scope(
        &self,
        scope: &str,
        unique_stops: &HashSet<BoardingPointId>,
        plan: &RealtimeRequestPlan,
        results: &mut RealtimeDepartureResults,
    ) {
        let mut stop_uids = unique_stops
            .iter()
            .map(|stop| stop.as_str().to_owned())
            .collect::<Vec<_>>();
        stop_uids.sort();
        let mut patterns_by_service = HashMap::<String, Vec<TransitPatternId>>::new();
        let mut parent_route_by_service = HashMap::<String, String>::new();
        for pattern_id in plan.services_by_scope.get(scope).into_iter().flatten() {
            if let Some(binding) = self.context.bindings.bus_patterns.get(pattern_id) {
                patterns_by_service
                    .entry(binding.tdx_service_id.clone())
                    .or_default()
                    .push(pattern_id.clone());
                if let Some(route_uid) = &binding.tdx_route_uid {
                    parent_route_by_service
                        .insert(binding.tdx_service_id.clone(), route_uid.clone());
                }
            }
        }
        let scope_pairs = plan.pairs_by_scope.get(scope).cloned().unwrap_or_default();
        let batch = self.context.live.bus_arrivals(scope, &stop_uids).await;
        ::metrics::counter!("via_bus_timing_realtime_http_requests_total")
            .increment(batch.upstream_requests as u64);
        results.http_requests += batch.upstream_requests;
        if !batch.unavailable.is_empty() {
            ::metrics::counter!("via_bus_timing_upstream_failures_total").increment(1);
        }
        if batch.is_rate_limited() {
            ::metrics::counter!("via_bus_timing_rate_limited_requests_total").increment(1);
        }
        let failed_stops = batch
            .unavailable
            .iter()
            .map(|unavailable| (unavailable.id.as_str(), unavailable.reason))
            .collect::<HashMap<&str, UnavailableReason>>();
        for pair in &scope_pairs {
            if let Some(reason) = failed_stops.get(pair.1.as_str()) {
                results
                    .errors_by_pair
                    .insert(pair.clone(), departure_error(*reason));
            }
        }
        let values = match parse_bus_arrivals(
            &batch.records,
            &patterns_by_service,
            &parent_route_by_service,
            &self.context.realtime_check,
        ) {
            Ok(values) => values,
            Err(error) => {
                ::metrics::counter!("via_bus_timing_upstream_failures_total").increment(1);
                for pair in scope_pairs {
                    results
                        .errors_by_pair
                        .entry(pair)
                        .or_insert_with(|| error.clone());
                }
                return;
            }
        };
        results.rows_received += values.len();
        for value in values {
            let arrival = &value.value;
            let pair = (
                arrival.transit_pattern_id.clone(),
                arrival.boarding_point_id.clone(),
            );
            if scope_pairs.contains(&pair) && !results.errors_by_pair.contains_key(&pair) {
                results.rows_relevant += 1;
                results.values_by_pair.entry(pair).or_default().push(value);
            }
        }
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
        query.validate_for(&self.context.transit, TransitMode::Bus)?;
        let mut departures = self.context.schedule.next_departures(query.clone()).await?;
        departures.extend(
            self.context
                .schedule
                .next_frequency_departures(query.clone())?,
        );
        let board_index = query.resolve_board_index(&self.context.transit)?;
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
        Some(self.realtime_departures(queries).await)
    }
}
