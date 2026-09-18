use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;

use super::source::TransitDepartureSource;
use crate::domain::{
    ids::EffectiveTripId,
    realtime::{ArrivalState, FreshnessState, NormalizedRealtimeValue, RealtimeValue},
    transit::{TransitMode, TransitNetwork},
    transit_timing::{
        DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
        ScheduleDepartureResolver, TimingReliability,
    },
};
use crate::providers::realtime::{ArrivalQuery, RealtimeError, RealtimeMetrics, RealtimeProvider};

/// Resolves TDX arrival observations into complete request-local effective trips.
const MAX_REALTIME_DEPARTURE_BATCH_PAIRS: usize = 64;

pub struct BusRealtimeResolver {
    realtime: Arc<dyn RealtimeProvider>,
    network: Arc<TransitNetwork>,
    metrics: RealtimeMetrics,
}

impl BusRealtimeResolver {
    pub fn new(realtime: Arc<dyn RealtimeProvider>, network: Arc<TransitNetwork>) -> Self {
        Self {
            realtime,
            network,
            metrics: RealtimeMetrics::default(),
        }
    }

    pub fn with_metrics(mut self, metrics: RealtimeMetrics) -> Self {
        self.metrics = metrics;
        self
    }

    fn effective_trip(
        &self,
        query: &DepartureQuery,
        departure_at: time::OffsetDateTime,
        observed_at: time::OffsetDateTime,
        evidence: crate::domain::realtime::NormalizedRealtimeValue,
    ) -> Option<EffectiveTrip> {
        let service = self.network.service(&query.service_id)?;
        let board_index = service
            .pattern
            .iter()
            .position(|stop_id| stop_id == &query.board_stop_id)?;
        let shape = self.network.representative_trip(&query.service_id);
        if shape.is_none() && self.network.has_invalid_timing_shape(&query.service_id) {
            return None;
        }
        let board_departure = shape.map(|trip| trip.stop_times[board_index].departure_seconds);
        let service_day = self.network.service_day(departure_at);
        let service_day_start = self.network.service_day_start(service_day);
        let mut stop_times = Vec::<EffectiveStopTime>::with_capacity(service.pattern.len());
        let mut previous_departure = None;
        for (index, stop_id) in service.pattern.iter().enumerate() {
            let stop_time = if index < board_index {
                shape
                    .map(|trip| EffectiveStopTime {
                        stop_id: stop_id.clone(),
                        arrival_at: service_day_start
                            + time::Duration::seconds(i64::from(
                                trip.stop_times[index].arrival_seconds,
                            )),
                        departure_at: service_day_start
                            + time::Duration::seconds(i64::from(
                                trip.stop_times[index].departure_seconds,
                            )),
                    })
                    .unwrap_or_else(|| EffectiveStopTime {
                        stop_id: stop_id.clone(),
                        arrival_at: departure_at,
                        departure_at,
                    })
            } else if let Some(shape) = shape {
                let board_departure = board_departure?;
                let arrival_offset = shape.stop_times[index]
                    .arrival_seconds
                    .checked_sub(board_departure)?;
                let departure_offset = shape.stop_times[index]
                    .departure_seconds
                    .checked_sub(board_departure)?;
                EffectiveStopTime {
                    stop_id: stop_id.clone(),
                    arrival_at: departure_at + time::Duration::seconds(i64::from(arrival_offset)),
                    departure_at: departure_at
                        + time::Duration::seconds(i64::from(departure_offset)),
                }
            } else if index == board_index {
                EffectiveStopTime {
                    stop_id: stop_id.clone(),
                    arrival_at: departure_at,
                    departure_at,
                }
            } else {
                let from = &service.pattern[index - 1];
                let duration =
                    self.network
                        .estimated_segment_seconds(&query.service_id, from, stop_id)?;
                let previous = previous_departure
                    .or_else(|| stop_times.last().map(|time| time.departure_at))?;
                let arrival_at = previous + time::Duration::seconds(i64::from(duration));
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
                query.service_id.as_str(),
                query.board_stop_id.as_str(),
                departure_at.unix_timestamp_nanos()
            )),
            service_id: query.service_id.clone(),
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
                if arrival.transit_service_id != query.service_id
                    || arrival.boarding_point_id != query.board_stop_id
                    || !matches!(
                        arrival.state,
                        ArrivalState::Predicted | ArrivalState::AtBoardingPoint
                    )
                {
                    return None;
                }
                let departure_at = arrival.predicted_arrival_at?;
                if departure_at < query.after {
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
}

#[async_trait]
impl DepartureResolver for BusRealtimeResolver {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.next_departures_batch(vec![query])
            .await
            .pop()
            .unwrap_or(Err(DepartureError::TemporarilyUnavailable))
    }

    async fn next_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
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

        let mut unique_pairs_by_scope = BTreeMap::<
            Option<String>,
            Vec<(
                crate::domain::ids::TransitServiceId,
                crate::domain::ids::BoardingPointId,
            )>,
        >::new();
        let mut seen_pairs = HashSet::new();
        for query in &active_queries {
            let pair = (query.service_id.clone(), query.board_stop_id.clone());
            if seen_pairs.insert(pair.clone()) {
                let scope = self
                    .network
                    .service(&query.service_id)
                    .and_then(|service| service.realtime_scope.clone());
                unique_pairs_by_scope.entry(scope).or_default().push(pair);
            }
        }
        for pairs in unique_pairs_by_scope.values_mut() {
            pairs.sort_by(|left, right| {
                left.0
                    .as_str()
                    .cmp(right.0.as_str())
                    .then(left.1.as_str().cmp(right.1.as_str()))
            });
        }
        let unique_pair_count = seen_pairs.len();
        let batch_count = unique_pairs_by_scope
            .values()
            .map(|pairs| pairs.len().div_ceil(MAX_REALTIME_DEPARTURE_BATCH_PAIRS))
            .sum();
        self.metrics
            .record_departure_batch(active_queries.len(), unique_pair_count, batch_count);

        let mut values_by_pair = HashMap::<
            (
                crate::domain::ids::TransitServiceId,
                crate::domain::ids::BoardingPointId,
            ),
            Vec<NormalizedRealtimeValue>,
        >::new();
        for (scope, unique_pairs) in unique_pairs_by_scope {
            for pairs in unique_pairs.chunks(MAX_REALTIME_DEPARTURE_BATCH_PAIRS) {
                let requested_pairs = pairs.iter().cloned().collect::<HashSet<_>>();
                let query = ArrivalQuery::new(
                    pairs.iter().map(|pair| pair.0.clone()),
                    pairs.iter().map(|pair| pair.1.clone()),
                )
                .with_optional_scope(scope.clone());
                let values = match self.realtime.arrival_estimates(query).await {
                    Ok(values) => values,
                    Err(error) => {
                        let error = map_departure_realtime_error(error);
                        return queries
                            .into_iter()
                            .map(|query| {
                                if query.limit == 0 {
                                    Ok(Vec::new())
                                } else {
                                    Err(error.clone())
                                }
                            })
                            .collect();
                    }
                };
                for value in values {
                    let RealtimeValue::ArrivalEstimate(arrival) = &value.value else {
                        continue;
                    };
                    let pair = (
                        arrival.transit_service_id.clone(),
                        arrival.boarding_point_id.clone(),
                    );
                    if requested_pairs.contains(&pair) {
                        values_by_pair.entry(pair).or_default().push(value);
                    }
                }
            }
        }

        queries
            .into_iter()
            .map(|query| {
                if query.limit == 0 {
                    Ok(Vec::new())
                } else {
                    let pair = (query.service_id.clone(), query.board_stop_id.clone());
                    Ok(self.resolve_query(
                        &query,
                        values_by_pair.get(&pair).map_or(&[], Vec::as_slice),
                    ))
                }
            })
            .collect()
    }
}

fn map_departure_realtime_error(error: RealtimeError) -> DepartureError {
    match error {
        RealtimeError::TimedOut => DepartureError::TimedOut,
        RealtimeError::RateLimited => DepartureError::RateLimited,
        RealtimeError::TemporarilyUnavailable => DepartureError::TemporarilyUnavailable,
        RealtimeError::RejectedPayload | RealtimeError::InvalidResponse => {
            DepartureError::InvalidResponse
        }
        RealtimeError::InvalidConfiguration => DepartureError::InvalidConfiguration,
    }
}

pub struct BusDepartureSource {
    realtime: Option<Arc<dyn DepartureResolver>>,
    schedule: ScheduleDepartureResolver,
}

impl BusDepartureSource {
    pub fn new(network: Arc<TransitNetwork>, realtime: Option<Arc<dyn DepartureResolver>>) -> Self {
        Self {
            realtime,
            schedule: ScheduleDepartureResolver::new(network),
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
        // The dispatcher normally guarantees this, but keeping the source
        // boundary explicit makes direct use safe as well.
        let Some(service) = self.schedule.network().service(&query.service_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if service.transit_mode != TransitMode::Bus
            || !service.pattern.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        self.schedule.next_departures(query).await
    }

    async fn realtime_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Option<Vec<Result<Vec<EffectiveTrip>, DepartureError>>> {
        let realtime = self.realtime.as_ref()?;
        Some(realtime.next_departures_batch(queries).await)
    }
}
