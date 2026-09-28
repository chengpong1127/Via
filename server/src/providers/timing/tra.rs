use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use async_trait::async_trait;
use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        realtime::{FreshnessPolicy, FreshnessState, LiveStatusTimes},
        transit::TransitMode,
        transit_runtime::TransitRuntime,
        transit_timing::{
            DepartureError, DepartureQuery, EffectiveTrip, ScheduleTiming, TimingBasis,
            TimingReliability,
        },
    },
    tdx::{TdxClient, TdxClientError},
};

use super::source::TransitDepartureSource;
use crate::providers::bindings::TdxProviderBindings;

const DEFAULT_TRAIN_LIVE_BOARD_PATH: &str = "v3/Rail/TRA/TrainLiveBoard";
const MAX_PROVIDER_CLOCK_SKEW: TimeDuration = TimeDuration::minutes(5);

#[derive(Debug, Deserialize)]
struct TdxTraTrainLiveBoardResponse {
    #[serde(rename = "UpdateTime")]
    update_time: String,
    #[serde(rename = "SrcUpdateTime")]
    source_update_time: String,
    #[serde(rename = "TrainLiveBoards")]
    train_live_boards: Vec<TdxTraTrainLiveBoard>,
}

#[derive(Debug, Deserialize)]
struct TdxTraTrainLiveBoard {
    #[serde(rename = "TrainNo")]
    train_no: Option<String>,
    #[serde(rename = "DelayTime")]
    delay_minutes: i64,
    #[serde(rename = "UpdateTime")]
    update_time: String,
}

#[derive(Clone, Copy)]
struct TraDelayObservation {
    delay: TimeDuration,
    observed_at: OffsetDateTime,
}

pub struct TraDepartureSource {
    network: Arc<TransitRuntime>,
    schedule: ScheduleTiming,
    tdx: Option<Arc<TdxClient>>,
    realtime_path: String,
    freshness_policy: FreshnessPolicy,
    bindings: Arc<TdxProviderBindings>,
}

impl TraDepartureSource {
    pub fn new(network: Arc<TransitRuntime>) -> Self {
        Self {
            schedule: ScheduleTiming::new(network.clone()),
            network,
            tdx: None,
            realtime_path: DEFAULT_TRAIN_LIVE_BOARD_PATH.into(),
            freshness_policy: FreshnessPolicy::tdx_one_minute("tra_train_live_board_v1"),
            bindings: Arc::new(TdxProviderBindings::default()),
        }
    }

    pub fn with_bindings(mut self, bindings: Arc<TdxProviderBindings>) -> Self {
        self.bindings = bindings;
        self
    }

    pub fn with_tdx(mut self, tdx: Option<Arc<TdxClient>>) -> Self {
        self.tdx = tdx;
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
        if self.network.route_of(pattern).mode != TransitMode::Rail
            || !pattern.stops.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        Ok(())
    }

    fn train_numbers(&self, pattern_id: &crate::domain::ids::TransitPatternId) -> Vec<String> {
        self.network
            .data()
            .trips
            .iter()
            .filter(|trip| &trip.pattern_id == pattern_id)
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

        let filter = train_filter(&requested_train_numbers);
        let request = [("$filter", filter.as_str()), ("$format", "JSON")];
        let payload = self
            .tdx
            .as_ref()
            .expect("realtime_departures requires a TDX client")
            .get_json::<TdxTraTrainLiveBoardResponse>(&self.realtime_path, &request)
            .await;
        let observations = match payload {
            Ok(payload) => {
                parse_realtime_payload(payload, &requested_train_numbers, &self.freshness_policy)
            }
            Err(error) => Err(map_tdx_error(error)),
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
                .network
                .data()
                .trips
                .iter()
                .filter(|trip| trip.pattern_id == query.pattern_id)
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
                (self.network.service_day(query.after)
                    == self.network.service_day(observation.observed_at))
                .then(|| self.delay_adjusted_trip(trip, query, observation))
                .flatten()
            })
            .collect::<Vec<_>>();
        let board_index = query.resolve_board_index(&self.network)?;
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
        let board_index = query.resolve_board_index(&self.network).ok()?;
        if trip.stop_times[board_index].departure_at < query.after
            || query
                .before
                .is_some_and(|before| trip.stop_times[board_index].departure_at > before)
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
        trip.evidence.clear();
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
        self.tdx.as_ref()?;
        Some(self.realtime_departures(queries).await)
    }
}

fn train_filter(train_numbers: &BTreeSet<String>) -> String {
    train_numbers
        .iter()
        .map(|train_no| format!("TrainNo eq '{}'", train_no.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn parse_realtime_payload(
    payload: TdxTraTrainLiveBoardResponse,
    requested_train_numbers: &BTreeSet<String>,
    freshness_policy: &FreshnessPolicy,
) -> Result<BTreeMap<String, TraDelayObservation>, DepartureError> {
    let received_at = OffsetDateTime::now_utc();
    let platform_update = parse_timestamp(&payload.update_time)?;
    let source_update = parse_timestamp(&payload.source_update_time)?;
    reject_future_timestamp(platform_update, received_at)?;
    reject_future_timestamp(source_update, received_at)?;
    let wrapper_evidence_at = platform_update.min(source_update);
    let wrapper_freshness = freshness_policy.evaluate(
        LiveStatusTimes {
            measured_at: Some(source_update),
            published_at: Some(wrapper_evidence_at),
            received_at,
            normalized_at: received_at,
        },
        received_at,
        None,
    );
    if wrapper_freshness.state != FreshnessState::Fresh {
        return Ok(BTreeMap::new());
    }

    let mut observations = BTreeMap::new();
    for board in payload.train_live_boards {
        let Some(train_no) = board.train_no.as_deref().map(str::trim) else {
            continue;
        };
        if train_no.is_empty() || !requested_train_numbers.contains(train_no) {
            continue;
        }
        if board.delay_minutes < 0 {
            continue;
        }
        let observed_at = parse_timestamp(&board.update_time)?;
        reject_future_timestamp(observed_at, received_at)?;
        let evidence_at = observed_at.min(wrapper_evidence_at);
        let freshness = freshness_policy.evaluate(
            LiveStatusTimes {
                measured_at: Some(observed_at),
                published_at: Some(evidence_at),
                received_at,
                normalized_at: received_at,
            },
            received_at,
            None,
        );
        if freshness.state != FreshnessState::Fresh {
            continue;
        }
        let delay_seconds = board
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
) -> Result<(), DepartureError> {
    if timestamp > received_at + MAX_PROVIDER_CLOCK_SKEW {
        return Err(DepartureError::InvalidResponse);
    }
    Ok(())
}

fn map_tdx_error(error: TdxClientError) -> DepartureError {
    match error {
        TdxClientError::InvalidConfiguration => DepartureError::InvalidConfiguration,
        TdxClientError::RateLimited | TdxClientError::RateLimitedWithRetryAfter(_) => {
            DepartureError::RateLimited
        }
        TdxClientError::OAuthRequest(error) | TdxClientError::ApiRequest(error)
            if error.is_timeout() =>
        {
            DepartureError::TimedOut
        }
        TdxClientError::OAuthRequest(_)
        | TdxClientError::InvalidOAuthResponse(_)
        | TdxClientError::ApiRequest(_)
        | TdxClientError::DownloadWrite(_)
        | TdxClientError::Credentials(_) => DepartureError::TemporarilyUnavailable,
        TdxClientError::ResponseTooLarge | TdxClientError::InvalidJson(_) => {
            DepartureError::InvalidResponse
        }
    }
}
