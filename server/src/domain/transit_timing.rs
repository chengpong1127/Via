//! Provider-neutral transit departure timing.
//!
//! Routing asks for usable departures through this contract. Schedule, live,
//! delay-adjusted, and estimated timing implementations all return the same
//! normalized result and keep their source-specific decisions behind the
//! provider boundary.

use std::sync::Arc;

use async_trait::async_trait;
use time::{Date, OffsetDateTime};

use super::{
    ids::{BoardingPointId, ScheduledRunId, TransitServiceId},
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
