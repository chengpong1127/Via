//! Provider adapters for demand-driven departure and BikeShare resolution.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;

use crate::{
    domain::{
        ids::{BikeShareStationId, EffectiveTripId},
        realtime::{ArrivalState, BikeAvailability, FreshnessState, RealtimeValue},
        transit::TransitNetwork,
        transit_timing::{
            DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
            ScheduleDepartureResolver,
        },
    },
    routing::transfers::{BikeAvailabilityResolver, TransferError},
};

use super::realtime::{ArrivalQuery, RealtimeError, RealtimeProvider};

/// Resolves TDX arrival observations into complete request-local effective trips.
pub struct RealtimeDepartureResolver {
    realtime: Arc<dyn RealtimeProvider>,
    network: Arc<TransitNetwork>,
}

impl RealtimeDepartureResolver {
    pub fn new(realtime: Arc<dyn RealtimeProvider>, network: Arc<TransitNetwork>) -> Self {
        Self { realtime, network }
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
        let shape = self.network.trips.iter().find(|trip| {
            trip.service_id == query.service_id && trip.stop_times.len() == service.pattern.len()
        });
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
                let ride =
                    self.network
                        .topology_ride(service_day, &query.service_id, from, stop_id)?;
                let duration: u32 = (ride.scheduled_arrival - ride.scheduled_departure)
                    .whole_seconds()
                    .try_into()
                    .ok()?;
                if duration == 0 {
                    return None;
                }
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
            scheduled_run_id: None,
            stop_times,
            timing_basis: crate::domain::transit_timing::TimingBasis::RealtimePrediction,
            observed_at: Some(observed_at),
            uncertainty_seconds: None,
            evidence: vec![evidence],
        })
    }
}

#[async_trait]
impl DepartureResolver for RealtimeDepartureResolver {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let values = self
            .realtime
            .arrival_estimates(ArrivalQuery::for_ride(
                query.service_id.clone(),
                query.board_stop_id.clone(),
            ))
            .await
            .map_err(map_departure_realtime_error)?;
        let mut departures = values
            .into_iter()
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
                self.effective_trip(&query, departure_at, arrival.transmitted_at, status)
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
        Ok(departures)
    }
}

/// Selects a timing adapter from provider-neutral service metadata.
pub struct CompositeDepartureResolver {
    network: Arc<TransitNetwork>,
    default: Arc<dyn DepartureResolver>,
    fallback: Arc<dyn DepartureResolver>,
    adapters: HashMap<String, Arc<dyn DepartureResolver>>,
}

impl CompositeDepartureResolver {
    pub fn with_schedule_fallback(
        network: Arc<TransitNetwork>,
        fallback_adapter: Arc<dyn DepartureResolver>,
    ) -> Self {
        Self {
            default: fallback_adapter.clone(),
            fallback: Arc::new(ScheduleDepartureResolver::new(network.clone())),
            network,
            adapters: HashMap::from([(String::from("bus_n1"), fallback_adapter)]),
        }
    }

    pub fn with_adapter(
        mut self,
        timing_source: impl Into<String>,
        adapter: Arc<dyn DepartureResolver>,
    ) -> Self {
        self.adapters.insert(timing_source.into(), adapter);
        self
    }
}

#[async_trait]
impl DepartureResolver for CompositeDepartureResolver {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        let Some(service) = self.network.service(&query.service_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        let Some(timing_source) = service.timing_source.as_deref() else {
            let scheduled = self.fallback.next_departures(query.clone()).await?;
            return if scheduled.is_empty() {
                self.default.next_departures(query).await
            } else {
                Ok(scheduled)
            };
        };
        let Some(adapter) = self.adapters.get(timing_source) else {
            let scheduled = self.fallback.next_departures(query.clone()).await?;
            return if scheduled.is_empty() {
                self.default.next_departures(query).await
            } else {
                Ok(scheduled)
            };
        };
        let preferred = adapter.next_departures(query.clone()).await;
        match preferred {
            Ok(departures) if !departures.is_empty() => Ok(departures),
            Ok(_) => self.fallback.next_departures(query).await,
            Err(preferred_error) => match self.fallback.next_departures(query).await {
                Ok(departures) if !departures.is_empty() => Ok(departures),
                Ok(_) | Err(_) => Err(preferred_error),
            },
        }
    }
}

/// Normalized realtime BikeShare availability for Trip-Based transfer checks.
pub struct RealtimeBikeAvailabilityResolver {
    realtime: Arc<dyn RealtimeProvider>,
}

impl RealtimeBikeAvailabilityResolver {
    pub fn new(realtime: Arc<dyn RealtimeProvider>) -> Self {
        Self { realtime }
    }
}

#[async_trait]
impl BikeAvailabilityResolver for RealtimeBikeAvailabilityResolver {
    async fn availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<HashMap<BikeShareStationId, BikeAvailability>, TransferError> {
        let values = self
            .realtime
            .bike_availability(station_ids)
            .await
            .map_err(map_transfer_realtime_error)?;
        Ok(values
            .into_iter()
            .filter_map(|status| {
                if status.freshness.state != FreshnessState::Fresh {
                    return None;
                }
                let RealtimeValue::BikeAvailability(availability) = status.value else {
                    return None;
                };
                Some((availability.station_id.clone(), availability))
            })
            .collect())
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

fn map_transfer_realtime_error(error: RealtimeError) -> TransferError {
    if error.is_transient() {
        TransferError::TemporarilyUnavailable
    } else {
        TransferError::InvalidResponse
    }
}
