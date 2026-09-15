//! Provider-neutral transit departure timing.
//!
//! Routing asks for usable departures through this contract. Schedule, live,
//! delay-adjusted, and estimated timing implementations all return the same
//! normalized result and keep their source-specific decisions behind the
//! provider boundary.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::join_all;
use time::{Date, OffsetDateTime};

use super::{
    ids::{BoardingPointId, EffectiveTripId, ScheduledRunId, TransitServiceId},
    realtime::NormalizedRealtimeValue,
    transit::{TransitNetwork, TransitRide},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingBasis {
    RealtimePrediction,
    DelayAdjustedSchedule,
    Schedule,
    FrequencyEstimate,
}

/// The only timing request a routing algorithm needs to make.
///
/// The caller supplies a service and the boarding point it has reached. It
/// does not need to know an alighting point or a pre-selected scheduled ride.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DepartureQuery {
    pub service_id: TransitServiceId,
    pub board_stop_id: BoardingPointId,
    pub after: OffsetDateTime,
    pub limit: usize,
}

impl DepartureQuery {
    pub fn new(
        service_id: TransitServiceId,
        board_stop_id: BoardingPointId,
        after: OffsetDateTime,
        limit: usize,
    ) -> Self {
        Self {
            service_id,
            board_stop_id,
            after,
            limit,
        }
    }
}

/// A concrete trip instance materialized for one routing request.
///
/// The stop sequence is inherited from the immutable service pattern. Runtime
/// data may replace only its effective times and evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveTrip {
    pub id: EffectiveTripId,
    pub service_id: TransitServiceId,
    pub scheduled_run_id: Option<ScheduledRunId>,
    pub stop_times: Vec<EffectiveStopTime>,
    pub timing_basis: TimingBasis,
    pub observed_at: Option<OffsetDateTime>,
    pub uncertainty_seconds: Option<u32>,
    pub evidence: Vec<NormalizedRealtimeValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveStopTime {
    pub stop_id: BoardingPointId,
    pub arrival_at: OffsetDateTime,
    pub departure_at: OffsetDateTime,
}

impl EffectiveTrip {
    pub fn stop_index(&self, stop_id: &BoardingPointId) -> Option<usize> {
        self.stop_times
            .iter()
            .position(|stop_time| &stop_time.stop_id == stop_id)
    }

    pub fn from_scheduled(
        network: &TransitNetwork,
        service_day: Date,
        trip_id: &ScheduledRunId,
    ) -> Option<Self> {
        let trip = network.trips.iter().find(|trip| &trip.id == trip_id)?;
        let service = network.service(&trip.service_id)?;
        if !trip.runs_on(service_day) || trip.stop_times.len() != service.pattern.len() {
            return None;
        }
        let start = network.service_day_start(service_day);
        let stop_times = service
            .pattern
            .iter()
            .zip(&trip.stop_times)
            .map(|(stop_id, stop_time)| EffectiveStopTime {
                stop_id: stop_id.clone(),
                arrival_at: start + time::Duration::seconds(i64::from(stop_time.arrival_seconds)),
                departure_at: start
                    + time::Duration::seconds(i64::from(stop_time.departure_seconds)),
            })
            .collect();
        Some(Self {
            id: EffectiveTripId::new(trip.id.as_str()),
            service_id: trip.service_id.clone(),
            scheduled_run_id: Some(trip.id.clone()),
            stop_times,
            timing_basis: TimingBasis::Schedule,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum DepartureError {
    #[error("departure resolver timed out")]
    TimedOut,
    #[error("departure resolver was rate limited")]
    RateLimited,
    #[error("departure resolver is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("departure resolver returned an invalid response")]
    InvalidResponse,
    #[error("departure resolver configuration is invalid")]
    InvalidConfiguration,
    #[error("departure query references an invalid service or boarding point")]
    InvalidQuery,
}

impl DepartureError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::TimedOut | Self::RateLimited | Self::TemporarilyUnavailable
        )
    }
}

/// Provider-facing departure boundary consumed by routing algorithms.
#[async_trait]
pub trait DepartureResolver: Send + Sync {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError>;

    /// Resolve a frontier in parallel. Implementations may override this to
    /// use a provider-native batch endpoint; the default preserves the same
    /// contract for simple adapters and deterministic fixtures.
    async fn next_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
        join_all(
            queries
                .into_iter()
                .map(|query| async move { self.next_departures(query).await }),
        )
        .await
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitTimingQuery {
    pub service_id: TransitServiceId,
    pub board_stop_id: BoardingPointId,
    pub alight_stop_id: BoardingPointId,
    /// Optional opaque key for selecting an operator/source adapter.
    pub timing_source: Option<String>,
    pub after: OffsetDateTime,
    pub limit: usize,
    /// The topology's static ride shape. It supplies the segment duration for
    /// live estimates and lets topology-only services work without a timetable
    /// row active on the current service day.
    pub scheduled_ride: TransitRide,
}

impl TransitTimingQuery {
    pub fn new(
        service_id: TransitServiceId,
        board_stop_id: BoardingPointId,
        alight_stop_id: BoardingPointId,
        after: OffsetDateTime,
        limit: usize,
        scheduled_ride: TransitRide,
    ) -> Self {
        Self {
            service_id,
            board_stop_id,
            alight_stop_id,
            timing_source: None,
            after,
            limit,
            scheduled_ride,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitDeparture {
    pub trip_id: Option<ScheduledRunId>,
    pub departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub timing_basis: TimingBasis,
    pub observed_at: Option<OffsetDateTime>,
    pub uncertainty_seconds: Option<u32>,
    pub evidence: Vec<NormalizedRealtimeValue>,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitTimingError {
    #[error("transit timing provider timed out")]
    TimedOut,
    #[error("transit timing provider rate limited the request")]
    RateLimited,
    #[error("transit timing provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("transit timing provider returned an invalid response")]
    InvalidResponse,
    #[error("transit timing provider configuration is invalid")]
    InvalidConfiguration,
    #[error("scheduled transit ride is invalid")]
    InvalidScheduledRide,
}

impl TransitTimingError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::TimedOut | Self::RateLimited | Self::TemporarilyUnavailable
        )
    }

    pub fn is_global(&self) -> bool {
        matches!(
            self,
            Self::InvalidResponse | Self::InvalidConfiguration | Self::InvalidScheduledRide
        )
    }
}

#[async_trait]
pub trait TransitTimingProvider: Send + Sync {
    async fn next_departures(
        &self,
        query: TransitTimingQuery,
    ) -> Result<Vec<TransitDeparture>, TransitTimingError>;
}

/// Resolve departures from Via's immutable static schedule.
#[derive(Clone)]
pub struct ScheduleTimingProvider {
    network: Arc<TransitNetwork>,
}

impl ScheduleTimingProvider {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        Self { network }
    }
}

/// Resolve complete scheduled trips without requiring an alighting stop.
///
/// This is intentionally a separate implementation from the compatibility
/// `ScheduleTimingProvider`: the latter serves the old ride-enrichment seam,
/// while this resolver is the input boundary for Trip-Based routing.
#[derive(Clone)]
pub struct ScheduleDepartureResolver {
    network: Arc<TransitNetwork>,
}

impl ScheduleDepartureResolver {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        Self { network }
    }

    pub fn network(&self) -> &Arc<TransitNetwork> {
        &self.network
    }
}

#[async_trait]
impl DepartureResolver for ScheduleDepartureResolver {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let Some(service) = self.network.service(&query.service_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if !service.pattern.contains(&query.board_stop_id) {
            return Err(DepartureError::InvalidQuery);
        }
        let board_index = service
            .pattern
            .iter()
            .position(|stop_id| stop_id == &query.board_stop_id)
            .ok_or(DepartureError::InvalidQuery)?;
        let service_day = self.network.service_day(query.after);
        let mut departures = self
            .network
            .trips
            .iter()
            .filter(|trip| trip.service_id == query.service_id && trip.runs_on(service_day))
            .filter_map(|trip| {
                let effective =
                    EffectiveTrip::from_scheduled(&self.network, service_day, &trip.id)?;
                (effective.stop_times[board_index].departure_at >= query.after).then_some(effective)
            })
            .collect::<Vec<_>>();
        departures.sort_by(|left, right| {
            left.stop_times[board_index]
                .departure_at
                .cmp(&right.stop_times[board_index].departure_at)
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        departures.truncate(query.limit);
        Ok(departures)
    }
}

#[async_trait]
impl TransitTimingProvider for ScheduleTimingProvider {
    async fn next_departures(
        &self,
        query: TransitTimingQuery,
    ) -> Result<Vec<TransitDeparture>, TransitTimingError> {
        if query.scheduled_ride.service_id != query.service_id
            || query.scheduled_ride.board_stop_id != query.board_stop_id
            || query.scheduled_ride.alight_stop_id != query.alight_stop_id
        {
            return Err(TransitTimingError::InvalidScheduledRide);
        }

        let service_day: Date = self.network.service_day(query.after);
        let mut departures = self
            .network
            .trips
            .iter()
            .filter(|trip| trip.service_id == query.service_id && trip.runs_on(service_day))
            .filter_map(|trip| {
                self.network.scheduled_ride(
                    service_day,
                    &query.service_id,
                    &trip.id,
                    &query.board_stop_id,
                    &query.alight_stop_id,
                )
            })
            .filter(|ride| ride.scheduled_departure >= query.after)
            .collect::<Vec<_>>();
        departures.sort_by(|left, right| {
            left.scheduled_departure
                .cmp(&right.scheduled_departure)
                .then(left.trip_id.as_str().cmp(right.trip_id.as_str()))
        });

        Ok(departures
            .into_iter()
            .take(query.limit)
            .map(|ride| TransitDeparture {
                trip_id: Some(ride.trip_id),
                departure_at: ride.scheduled_departure,
                arrival_at: ride.scheduled_arrival,
                timing_basis: TimingBasis::Schedule,
                observed_at: None,
                uncertainty_seconds: None,
                evidence: Vec::new(),
            })
            .collect())
    }
}
