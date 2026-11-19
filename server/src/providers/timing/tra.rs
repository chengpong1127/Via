use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use async_trait::async_trait;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        realtime::{
            ArrivalEstimate, ArrivalState, FreshnessPolicy, FreshnessState, LiveStatusTimes,
            NormalizedRealtimeValue,
        },
        transit::network::TransitMode,
        transit::runtime::TransitRuntime,
        transit::timing::{
            DepartureError, DepartureQuery, EffectiveTrip, ScheduleTiming, TimingBasis,
            TimingReliability,
        },
    },
    live::{CachedLiveData, DirectLiveData, LiveDataSource, TraDelay},
    tdx::{RealtimeSettings, TdxClient},
};

use super::{live_error::departure_error, source::TransitDepartureSource};
use crate::providers::tdx_bindings::TdxProviderBindings;

#[derive(Clone, Copy)]
struct TraDelayObservation {
    delay: TimeDuration,
    observed_at: OffsetDateTime,
}

pub struct TraDepartureSource {
    transit: Arc<TransitRuntime>,
    schedule: ScheduleTiming,
    live: Arc<dyn LiveDataSource>,
    freshness_policy: FreshnessPolicy,
    max_clock_skew_seconds: u32,
    bindings: Arc<TdxProviderBindings>,
}

impl TraDepartureSource {
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
        if self.transit.required_route(&pattern.route_id).mode != TransitMode::Rail
            || !pattern.stops.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        Ok(())
    }

    fn train_numbers(&self, pattern_id: &crate::domain::ids::TransitPatternId) -> Vec<String> {
        self.transit
            .scheduled_trips_of(pattern_id)
            .filter_map(|trip| self.bindings.tra_trips.get(&trip.id))
            .map(|binding| binding.train_number.clone())
            .collect()
    }

    async fn realtime_departures(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
        let mut requested_train_numbers = BTreeSet::new();
        let mut invalid_queries = BTreeSet::new();
        for (index, query) in queries.iter().enumerate() {
            if query.limit == 0 {
                continue;
            }
            if self.validate_query(query).is_err() {
                invalid_queries.insert(index);
                continue;
            }
            requested_train_numbers.extend(self.train_numbers(&query.pattern_id));
        }

        if requested_train_numbers.is_empty() {
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

        let train_numbers = requested_train_numbers.iter().cloned().collect::<Vec<_>>();
        let batch = self.live.tra_delays(&train_numbers).await;
        let observations = match batch.unavailable.first() {
            Some(unavailable) if batch.records.is_empty() => {
                Err(departure_error(unavailable.reason))
            }
            _ => parse_tra_delays(
                batch.records,
                &requested_train_numbers,
                &self.freshness_policy,
                self.max_clock_skew_seconds,
            ),
        };

        match observations {
            Ok(observations) => {
                let mut results = Vec::with_capacity(queries.len());
                for (index, query) in queries.into_iter().enumerate() {
                    if invalid_queries.contains(&index) {
                        results.push(Err(DepartureError::InvalidQuery));
                        continue;
                    }
                    if query.limit == 0 {
                        results.push(Ok(Vec::new()));
                        continue;
                    }
                    results.push(self.resolve_query(&query, &observations).await);
                }
                results
            }
            Err(error) => queries
                .into_iter()
                .enumerate()
                .map(|(index, query)| {
                    if invalid_queries.contains(&index) {
                        Err(DepartureError::InvalidQuery)
                    } else if query.limit == 0 || self.train_numbers(&query.pattern_id).is_empty() {
                        Ok(Vec::new())
                    } else {
                        Err(error.clone())
                    }
                })
                .collect(),
        }
    }

    async fn resolve_query(
        &self,
        query: &DepartureQuery,
        observations: &BTreeMap<String, TraDelayObservation>,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        if observations.is_empty() {
            return Ok(Vec::new());
        }
        let max_delay = observations
            .values()
            .map(|observation| observation.delay)
            .max()
            .unwrap_or(TimeDuration::ZERO);
        let adjusted_after = query.after.checked_sub(max_delay).unwrap_or(query.after);
        let static_query = DepartureQuery {
            after: adjusted_after,
            limit: self
                .transit
                .scheduled_trips_of(&query.pattern_id)
                .count()
                .max(query.limit),
            ..query.clone()
        };
        let scheduled = self.schedule.next_departures(static_query).await?;
        let mut departures = scheduled
            .into_iter()
            .filter_map(|trip| {
                let trip_id = trip.scheduled_trip_id.as_ref()?;
                let train_number = &self.bindings.tra_trips.get(trip_id)?.train_number;
                let observation = *observations.get(train_number)?;
                (self.transit.service_day(query.after)
                    == self.transit.service_day(observation.observed_at))
                .then(|| self.delay_adjusted_trip(trip, query, observation))
                .flatten()
            })
            .collect::<Vec<_>>();
        let board_index = query.resolve_board_index(&self.transit)?;
        departures.sort_by(|left, right| {
            left.stop_times[board_index]
                .departure_at
                .cmp(&right.stop_times[board_index].departure_at)
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        departures.truncate(query.limit);
        Ok(departures)
    }

    fn delay_adjusted_trip(
        &self,
        mut trip: EffectiveTrip,
        query: &DepartureQuery,
        observation: TraDelayObservation,
    ) -> Option<EffectiveTrip> {
        for stop_time in &mut trip.stop_times {
            stop_time.arrival_at = stop_time.arrival_at.checked_add(observation.delay)?;
            stop_time.departure_at = stop_time.departure_at.checked_add(observation.delay)?;
        }
        let board_index = query.resolve_board_index(&self.transit).ok()?;
        let board_departure = trip.stop_times[board_index].departure_at;
        if board_departure < query.after
            || query.before.is_some_and(|before| board_departure > before)
        {
            return None;
        }
        let scheduled_trip_id = trip.scheduled_trip_id.clone();
        trip.id = crate::domain::ids::EffectiveTripId::new(format!(
            "tra:realtime:{}:{}",
            trip.id.as_str(),
            observation.observed_at.unix_timestamp_nanos()
        ));
        trip.timing_basis = TimingBasis::DelayAdjustedSchedule;
        trip.timing_reliability = TimingReliability::Reliable;
        trip.scheduled_trip_id = scheduled_trip_id;
        trip.observed_at = Some(observation.observed_at);
        trip.uncertainty_seconds = None;
        trip.evidence = vec![NormalizedRealtimeValue {
            freshness: FreshnessState::Fresh,
            value: ArrivalEstimate {
                transit_pattern_id: trip.pattern_id.clone(),
                boarding_point_id: query.board_stop_id.clone(),
                predicted_arrival_at: Some(board_departure),
                state: ArrivalState::Predicted,
                transmitted_at: observation.observed_at,
            },
        }];
        Some(trip)
    }
}

#[async_trait]
impl TransitDepartureSource for TraDepartureSource {
    fn mode(&self) -> TransitMode {
        TransitMode::Rail
    }

    fn static_timing_reliability(&self, _query: &DepartureQuery) -> TimingReliability {
        TimingReliability::Reliable
    }

    async fn static_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.validate_query(&query)?;
        self.schedule.next_departures(query).await
    }

    async fn realtime_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Option<Vec<Result<Vec<EffectiveTrip>, DepartureError>>> {
        Some(self.realtime_departures(queries).await)
    }
}

fn parse_tra_delays(
    delays: Vec<TraDelay>,
    requested_train_numbers: &BTreeSet<String>,
    freshness_policy: &FreshnessPolicy,
    max_clock_skew_seconds: u32,
) -> Result<BTreeMap<String, TraDelayObservation>, DepartureError> {
    let received_at = OffsetDateTime::now_utc();
    let mut observations = BTreeMap::new();
    for delay in delays {
        let platform_update = parse_timestamp(&delay.board_update_time)?;
        let source_update = parse_timestamp(&delay.board_source_update_time)?;
        reject_future_timestamp(platform_update, received_at, max_clock_skew_seconds)?;
        reject_future_timestamp(source_update, received_at, max_clock_skew_seconds)?;
        let board_evidence_at = platform_update.min(source_update);
        let train_no = delay.train_no.trim();
        if train_no.is_empty() || !requested_train_numbers.contains(train_no) {
            continue;
        }
        if delay.delay_minutes < 0 {
            continue;
        }
        let observed_at = parse_timestamp(&delay.update_time)?;
        reject_future_timestamp(observed_at, received_at, max_clock_skew_seconds)?;
        let evidence_at = observed_at.min(board_evidence_at);
        let freshness = freshness_policy.evaluate(
            LiveStatusTimes {
                published_at: Some(evidence_at),
                received_at,
            },
            received_at,
        );
        if freshness != FreshnessState::Fresh {
            continue;
        }
        let delay_seconds = delay
            .delay_minutes
            .checked_mul(60)
            .ok_or(DepartureError::InvalidResponse)?;
        let observation = TraDelayObservation {
            delay: TimeDuration::seconds(delay_seconds),
            observed_at,
        };
        let replace = observations
            .get(train_no)
            .is_none_or(|current: &TraDelayObservation| current.observed_at < observed_at);
        if replace {
            observations.insert(train_no.to_owned(), observation);
        }
    }
    Ok(observations)
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, DepartureError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| DepartureError::InvalidResponse)
}

fn reject_future_timestamp(
    timestamp: OffsetDateTime,
    received_at: OffsetDateTime,
    max_clock_skew_seconds: u32,
) -> Result<(), DepartureError> {
    if timestamp > received_at + TimeDuration::seconds(i64::from(max_clock_skew_seconds)) {
        return Err(DepartureError::InvalidResponse);
    }
    Ok(())
}
