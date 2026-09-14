//! Transit timing adapters and composition.
//!
//! The routing pipeline depends only on `domain::transit_timing`. This module
//! adapts the existing normalized realtime provider and composes it with the
//! immutable schedule fallback.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;

use crate::domain::{
    realtime::{ArrivalState, RealtimeValue},
    transit_timing::{
        ScheduleTimingProvider, TransitDeparture, TransitTimingError, TransitTimingProvider,
        TransitTimingQuery,
    },
};

use super::realtime::{ArrivalQuery, RealtimeError, RealtimeProvider};

/// Adapts normalized arrival observations into the provider-neutral departure
/// contract. It deliberately does not decide which transit mode is eligible
/// for this source; that is a provider/composition concern.
pub struct RealtimeTransitTimingAdapter {
    realtime: Arc<dyn RealtimeProvider>,
}

impl RealtimeTransitTimingAdapter {
    pub fn new(realtime: Arc<dyn RealtimeProvider>) -> Self {
        Self { realtime }
    }
}

#[async_trait]
impl TransitTimingProvider for RealtimeTransitTimingAdapter {
    async fn next_departures(
        &self,
        query: TransitTimingQuery,
    ) -> Result<Vec<TransitDeparture>, TransitTimingError> {
        let values = self
            .realtime
            .arrival_estimates(ArrivalQuery::for_ride(
                query.service_id.clone(),
                query.board_stop_id.clone(),
            ))
            .await
            .map_err(map_realtime_error)?;

        let ride_duration = (query.scheduled_ride.scheduled_arrival
            - query.scheduled_ride.scheduled_departure)
            .whole_seconds()
            .max(1);
        let mut departures = values
            .into_iter()
            .filter_map(|status| {
                if status.freshness.state != crate::domain::realtime::FreshnessState::Fresh {
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
                (departure_at >= query.after).then_some(TransitDeparture {
                    trip_id: None,
                    departure_at,
                    arrival_at: departure_at + time::Duration::seconds(ride_duration),
                    timing_basis: crate::domain::transit_timing::TimingBasis::RealtimePrediction,
                    observed_at: Some(arrival.transmitted_at),
                    uncertainty_seconds: None,
                    evidence: vec![status],
                })
            })
            .collect::<Vec<_>>();
        departures.sort_by_key(|departure| departure.departure_at);
        departures.truncate(query.limit);
        Ok(departures)
    }
}

/// Compose a preferred timing source with the immutable schedule source.
/// Empty results and source failures both fall back deliberately; a failure is
/// returned only when no fallback departure can be resolved.
pub struct CompositeTransitTimingProvider {
    preferred: Arc<dyn TransitTimingProvider>,
    fallback: Arc<dyn TransitTimingProvider>,
    adapters: HashMap<String, Arc<dyn TransitTimingProvider>>,
}

impl CompositeTransitTimingProvider {
    pub fn new(
        preferred: Arc<dyn TransitTimingProvider>,
        fallback: Arc<dyn TransitTimingProvider>,
    ) -> Self {
        Self {
            preferred,
            fallback,
            adapters: HashMap::new(),
        }
    }

    pub fn with_schedule_fallback(
        preferred: Arc<dyn TransitTimingProvider>,
        network: Arc<crate::domain::transit::TransitNetwork>,
    ) -> Self {
        let schedule = Arc::new(ScheduleTimingProvider::new(network));
        Self::new(preferred, schedule.clone()).with_adapter("schedule", schedule)
    }

    pub fn with_adapter(
        mut self,
        timing_source: impl Into<String>,
        adapter: Arc<dyn TransitTimingProvider>,
    ) -> Self {
        self.adapters.insert(timing_source.into(), adapter);
        self
    }
}

#[async_trait]
impl TransitTimingProvider for CompositeTransitTimingProvider {
    async fn next_departures(
        &self,
        query: TransitTimingQuery,
    ) -> Result<Vec<TransitDeparture>, TransitTimingError> {
        let preferred_provider: &dyn TransitTimingProvider = match query.timing_source.as_deref() {
            Some(source) => self
                .adapters
                .get(source)
                .map(Arc::as_ref)
                .unwrap_or_else(|| self.fallback.as_ref()),
            None => self.preferred.as_ref(),
        };
        let preferred = preferred_provider.next_departures(query.clone()).await;
        match preferred {
            Ok(departures) if !departures.is_empty() => Ok(departures),
            Ok(_) => self.fallback.next_departures(query).await,
            Err(preferred_error) => match self.fallback.next_departures(query).await {
                Ok(departures) if !departures.is_empty() => Ok(departures),
                Ok(_) => Err(preferred_error),
                Err(_) => Err(preferred_error),
            },
        }
    }
}

fn map_realtime_error(error: RealtimeError) -> TransitTimingError {
    match error {
        RealtimeError::TimedOut => TransitTimingError::TimedOut,
        RealtimeError::RateLimited => TransitTimingError::RateLimited,
        RealtimeError::TemporarilyUnavailable => TransitTimingError::TemporarilyUnavailable,
        RealtimeError::RejectedPayload | RealtimeError::InvalidResponse => {
            TransitTimingError::InvalidResponse
        }
        RealtimeError::InvalidConfiguration => TransitTimingError::InvalidConfiguration,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::macros::{date, datetime};

    use super::*;
    use crate::domain::{
        ids::{BoardingPointId, ScheduledRunId, TransitServiceId},
        realtime::{
            ArrivalEstimate, ArrivalState, Freshness, FreshnessState, NormalizedRealtimeValue,
        },
        transit::{
            ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitRide,
            TransitService, TransitStop,
        },
    };

    #[derive(Clone)]
    struct FixtureRealtime {
        values: Vec<NormalizedRealtimeValue>,
    }

    #[async_trait]
    impl RealtimeProvider for FixtureRealtime {
        async fn arrival_estimates(
            &self,
            _query: ArrivalQuery,
        ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
            Ok(self.values.clone())
        }

        async fn bike_availability(
            &self,
            _station_ids: &[crate::domain::ids::BikeShareStationId],
        ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
            Ok(Vec::new())
        }

        async fn service_conditions(
            &self,
            _service_ids: &[TransitServiceId],
        ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
            Ok(Vec::new())
        }
    }

    fn query(mode: TransitMode) -> TransitTimingQuery {
        let service_id = TransitServiceId::new("service");
        let board_stop_id = BoardingPointId::new("board");
        let alight_stop_id = BoardingPointId::new("alight");
        TransitTimingQuery {
            service_id,
            board_stop_id,
            alight_stop_id,
            timing_source: None,
            after: datetime!(2026-08-31 09:00 +08:00),
            limit: 3,
            scheduled_ride: TransitRide {
                transit_mode: mode,
                service_id: TransitServiceId::new("service"),
                trip_id: ScheduledRunId::new("shape"),
                board_stop_id: BoardingPointId::new("board"),
                alight_stop_id: BoardingPointId::new("alight"),
                scheduled_departure: datetime!(2026-08-31 09:00 +08:00),
                scheduled_arrival: datetime!(2026-08-31 09:10 +08:00),
            },
        }
    }

    fn fresh_arrival() -> NormalizedRealtimeValue {
        let received_at = datetime!(2026-08-31 08:59 +08:00);
        NormalizedRealtimeValue {
            id: "arrival".into(),
            received_at,
            normalized_at: received_at,
            provider_published_at: Some(received_at),
            source_change_at: None,
            freshness: Freshness {
                state: FreshnessState::Fresh,
                reason: None,
                policy_version: "test".into(),
                evaluated_at: received_at,
            },
            value: RealtimeValue::ArrivalEstimate(ArrivalEstimate {
                transit_service_id: TransitServiceId::new("service"),
                boarding_point_id: BoardingPointId::new("board"),
                predicted_arrival_at: Some(datetime!(2026-08-31 09:05 +08:00)),
                state: ArrivalState::Predicted,
                transmitted_at: received_at,
            }),
        }
    }

    #[tokio::test]
    async fn normalized_realtime_departures_are_mode_agnostic() {
        for mode in [
            TransitMode::Bus,
            TransitMode::Metro,
            TransitMode::Rail,
            TransitMode::HighSpeedRail,
        ] {
            let provider = RealtimeTransitTimingAdapter::new(Arc::new(FixtureRealtime {
                values: vec![fresh_arrival()],
            }));
            let departures = provider.next_departures(query(mode)).await.unwrap();
            assert_eq!(departures.len(), 1);
            assert_eq!(
                departures[0].departure_at,
                datetime!(2026-08-31 09:05 +08:00)
            );
            assert_eq!(departures[0].arrival_at, datetime!(2026-08-31 09:15 +08:00));
            assert_eq!(
                departures[0].timing_basis,
                crate::domain::transit_timing::TimingBasis::RealtimePrediction
            );
        }
    }

    #[tokio::test]
    async fn multiple_usable_realtime_departures_remain_chronological() {
        let mut second = fresh_arrival();
        second.id = "arrival-2".into();
        if let RealtimeValue::ArrivalEstimate(arrival) = &mut second.value {
            arrival.predicted_arrival_at = Some(datetime!(2026-08-31 09:12 +08:00));
        }
        let provider = RealtimeTransitTimingAdapter::new(Arc::new(FixtureRealtime {
            values: vec![second, fresh_arrival()],
        }));
        let departures = provider
            .next_departures(query(TransitMode::Bus))
            .await
            .unwrap();
        assert_eq!(
            departures
                .iter()
                .map(|departure| departure.departure_at)
                .collect::<Vec<_>>(),
            vec![
                datetime!(2026-08-31 09:05 +08:00),
                datetime!(2026-08-31 09:12 +08:00)
            ]
        );
    }

    #[tokio::test]
    async fn schedule_fallback_is_used_when_live_timing_has_no_usable_departure() {
        let board = BoardingPointId::new("board");
        let alight = BoardingPointId::new("alight");
        let service_id = TransitServiceId::new("service");
        let network = Arc::new(TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![
                TransitStop::new(
                    board.clone(),
                    "Board",
                    crate::api::RequestCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                ),
                TransitStop::new(
                    alight.clone(),
                    "Alight",
                    crate::api::RequestCoordinate {
                        latitude: 25.01,
                        longitude: 121.51,
                    },
                ),
            ],
            vec![TransitService::new(
                service_id.clone(),
                TransitMode::Rail,
                "Rail",
                "Alight",
                vec![board.clone(), alight.clone()],
            )],
            vec![ScheduledTrip::weekdays(
                ScheduledRunId::new("run"),
                service_id.clone(),
                vec![time::Weekday::Monday],
                vec![
                    ScheduledStopTime::new(36_000, 36_000),
                    ScheduledStopTime::new(36_600, 36_600),
                ],
            )],
            Vec::new(),
        ));
        let ride = network
            .scheduled_ride(
                date!(2026 - 08 - 31),
                &service_id,
                &ScheduledRunId::new("run"),
                &board,
                &alight,
            )
            .unwrap();
        let provider = CompositeTransitTimingProvider::with_schedule_fallback(
            Arc::new(RealtimeTransitTimingAdapter::new(Arc::new(
                FixtureRealtime { values: Vec::new() },
            ))),
            network,
        );
        let departures = provider
            .next_departures(TransitTimingQuery {
                service_id,
                board_stop_id: board,
                alight_stop_id: alight,
                timing_source: None,
                after: datetime!(2026-08-31 09:00 +08:00),
                limit: 3,
                scheduled_ride: ride,
            })
            .await
            .unwrap();
        assert_eq!(departures.len(), 1);
        assert_eq!(
            departures[0].timing_basis,
            crate::domain::transit_timing::TimingBasis::Schedule
        );
    }
}
