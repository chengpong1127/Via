//! Phase 2: resolve the departures needed by the relevant transit backbone.
//! The resolver is mode-generic at the planner boundary; only this module
//! knows that buses need Arrival Estimates while rail-like services use the
//! immutable schedule.

use std::collections::HashMap;

use async_trait::async_trait;
use time::{Date, OffsetDateTime};

use crate::{
    live_status::FreshnessState,
    normalization::{ArrivalState, BoardingPointId, TransitServiceId},
    realtime::{NormalizedRealtimeValue, RealtimeValue},
    transit::{
        TransitItinerary, TransitMode, TransitNetwork, TransitRide, TransitService,
        TransitTopologyItinerary,
    },
};

use super::{PlanningContext, candidate_discovery::TransitCandidateNetwork};

#[derive(Clone, Debug, Default)]
pub struct RealtimeContext {
    arrivals: HashMap<(TransitServiceId, BoardingPointId), Vec<NormalizedRealtimeValue>>,
    bikes: HashMap<crate::normalization::BikeShareStationId, NormalizedRealtimeValue>,
    conditions: HashMap<TransitServiceId, NormalizedRealtimeValue>,
}

pub fn requires_realtime_arrival(ride: &TransitRide) -> bool {
    matches!(ride.transit_mode, TransitMode::Bus)
}

impl RealtimeContext {
    pub fn from_values(values: impl IntoIterator<Item = NormalizedRealtimeValue>) -> Self {
        let mut context = Self::default();
        context.extend(values);
        context
    }

    pub fn extend(&mut self, values: impl IntoIterator<Item = NormalizedRealtimeValue>) {
        for value in values {
            match &value.value {
                RealtimeValue::ArrivalEstimate(arrival) => {
                    self.arrivals
                        .entry((
                            arrival.transit_service_id.clone(),
                            arrival.boarding_point_id.clone(),
                        ))
                        .or_default()
                        .push(value);
                }
                RealtimeValue::BikeAvailability(availability) => {
                    self.bikes.insert(availability.station_id.clone(), value);
                }
                RealtimeValue::ServiceCondition(condition) => {
                    for service_id in &condition.scope_via_ids {
                        self.conditions
                            .insert(TransitServiceId::new(service_id), value.clone());
                    }
                }
            }
        }
        for values in self.arrivals.values_mut() {
            values.sort_by_key(|value| match &value.value {
                RealtimeValue::ArrivalEstimate(arrival) => arrival.predicted_arrival_at,
                _ => None,
            });
        }
    }

    pub fn arrivals_for(
        &self,
        service_id: &TransitServiceId,
        boarding_point_id: &BoardingPointId,
    ) -> &[NormalizedRealtimeValue] {
        self.arrivals
            .get(&(service_id.clone(), boarding_point_id.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn next_arrival(
        &self,
        service_id: &TransitServiceId,
        boarding_point_id: &BoardingPointId,
        after: OffsetDateTime,
    ) -> Option<&NormalizedRealtimeValue> {
        self.next_arrivals(service_id, boarding_point_id, after)
            .into_iter()
            .next()
    }

    pub fn next_arrivals(
        &self,
        service_id: &TransitServiceId,
        boarding_point_id: &BoardingPointId,
        after: OffsetDateTime,
    ) -> Vec<&NormalizedRealtimeValue> {
        let mut arrivals = self
            .arrivals_for(service_id, boarding_point_id)
            .iter()
            .filter(|value| value.freshness.state != FreshnessState::Unavailable)
            .filter_map(|value| {
                let RealtimeValue::ArrivalEstimate(arrival) = &value.value else {
                    return None;
                };
                let predicted = arrival.predicted_arrival_at?;
                (predicted >= after).then_some((predicted, value))
            })
            .collect::<Vec<_>>();
        arrivals.sort_by_key(|(predicted, _)| *predicted);
        arrivals
            .into_iter()
            .take(3)
            .map(|(_, value)| value)
            .collect()
    }

    pub fn bike(
        &self,
        station_id: &crate::normalization::BikeShareStationId,
    ) -> Option<&NormalizedRealtimeValue> {
        self.bikes.get(station_id)
    }

    pub fn station_ids(&self) -> impl Iterator<Item = &crate::normalization::BikeShareStationId> {
        self.bikes.keys()
    }

    pub fn condition(&self, service_id: &TransitServiceId) -> Option<&NormalizedRealtimeValue> {
        self.conditions.get(service_id)
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedDeparture {
    pub ride: TransitRide,
    pub departure: OffsetDateTime,
    pub arrival: OffsetDateTime,
    pub live_status: Option<NormalizedRealtimeValue>,
    pub schedule_only: bool,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitTimingError {
    #[error("scheduled transit ride is invalid")]
    InvalidScheduledRide,
}

#[async_trait]
pub trait TransitDepartureResolver: Send + Sync {
    async fn next_departures(
        &self,
        service: &TransitService,
        boarding_point: &BoardingPointId,
        after: OffsetDateTime,
        scheduled: &TransitRide,
        realtime: &RealtimeContext,
    ) -> Result<Vec<ResolvedDeparture>, TransitTimingError>;
}

pub struct NetworkTransitDepartureResolver<'a> {
    network: &'a TransitNetwork,
}

impl<'a> NetworkTransitDepartureResolver<'a> {
    pub fn new(network: &'a TransitNetwork) -> Self {
        Self { network }
    }
}

#[async_trait]
impl TransitDepartureResolver for NetworkTransitDepartureResolver<'_> {
    async fn next_departures(
        &self,
        service: &TransitService,
        boarding_point: &BoardingPointId,
        after: OffsetDateTime,
        scheduled: &TransitRide,
        realtime: &RealtimeContext,
    ) -> Result<Vec<ResolvedDeparture>, TransitTimingError> {
        match service.transit_mode {
            TransitMode::Bus => Ok(realtime
                .next_arrivals(&service.id, boarding_point, after)
                .into_iter()
                .filter(|status| status.freshness.state == FreshnessState::Fresh)
                .filter_map(|status| {
                    let RealtimeValue::ArrivalEstimate(arrival) = &status.value else {
                        return None;
                    };
                    if !matches!(
                        arrival.state,
                        ArrivalState::Predicted | ArrivalState::AtBoardingPoint
                    ) {
                        return None;
                    }
                    let predicted = arrival.predicted_arrival_at?;
                    let ride_duration = (scheduled.scheduled_arrival
                        - scheduled.scheduled_departure)
                        .whole_seconds()
                        .max(1);
                    let adjusted_arrival = predicted + time::Duration::seconds(ride_duration);
                    Some(ResolvedDeparture {
                        ride: scheduled.clone(),
                        departure: predicted,
                        arrival: adjusted_arrival,
                        live_status: Some(status.clone()),
                        schedule_only: false,
                    })
                })
                .collect()),
            TransitMode::Metro | TransitMode::Rail | TransitMode::HighSpeedRail => {
                let service_day = self.network.service_day(after);
                let mut departures = self
                    .network
                    .trips
                    .iter()
                    .filter(|trip| trip.service_id == service.id && trip.runs_on(service_day))
                    .filter_map(|trip| {
                        self.network.scheduled_ride(
                            service_day,
                            &service.id,
                            &trip.id,
                            boarding_point,
                            &scheduled.alight_stop_id,
                        )
                    })
                    .filter(|ride| ride.scheduled_departure >= after)
                    .collect::<Vec<_>>();
                departures.sort_by(|left, right| {
                    left.scheduled_departure
                        .cmp(&right.scheduled_departure)
                        .then(left.trip_id.as_str().cmp(right.trip_id.as_str()))
                });
                Ok(departures
                    .into_iter()
                    .take(3)
                    .map(|ride| ResolvedDeparture {
                        departure: ride.scheduled_departure,
                        arrival: ride.scheduled_arrival,
                        ride,
                        live_status: None,
                        schedule_only: !realtime.condition(&service.id).is_some_and(|condition| {
                            condition.freshness.state == FreshnessState::Fresh
                        }),
                    })
                    .collect())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimedTransitRide {
    pub ride: TransitRide,
    pub departure: OffsetDateTime,
    pub arrival: OffsetDateTime,
    pub evidence: Vec<NormalizedRealtimeValue>,
    pub required_states: Vec<FreshnessState>,
    pub schedule_only: bool,
}

#[derive(Clone, Debug)]
pub struct TimedTransitItinerary {
    pub service_day: Date,
    pub access_departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub rides: Vec<TimedTransitRide>,
}

#[derive(Clone, Debug, Default)]
pub struct TimedTransitBackbone {
    pub itineraries: Vec<TimedTransitItinerary>,
}

pub(crate) async fn resolve_backbone(
    network: &TransitNetwork,
    candidate: &TransitCandidateNetwork,
    static_itineraries: &[crate::transit::TransitItinerary],
    realtime: &RealtimeContext,
    context: &PlanningContext,
) -> TimedTransitBackbone {
    let resolver = NetworkTransitDepartureResolver::new(network);
    let mut timed = Vec::new();
    for itinerary in static_itineraries {
        let Some(first) = itinerary.rides.first() else {
            continue;
        };
        let Some(_access) = candidate.access_for(&first.board_stop_id) else {
            continue;
        };
        // Keep the static backbone broad enough to include a departure that a
        // local BikeShare connector can catch.  The search phase validates
        // whether the selected walking/BikeShare access actually reaches the
        // boarding point before this departure.
        #[derive(Clone)]
        struct Partial {
            available_at: OffsetDateTime,
            rides: Vec<TimedTransitRide>,
        }

        let mut partials = vec![Partial {
            available_at: context.search_start,
            rides: Vec::new(),
        }];
        for (index, ride) in itinerary.rides.iter().enumerate() {
            let Some(service) = network.service(&ride.service_id) else {
                break;
            };
            let mut next_partials = Vec::new();
            for partial in partials {
                let mut available_at = partial.available_at;
                if index > 0 {
                    let Some(previous) = partial.rides.last() else {
                        continue;
                    };
                    let Some(transfer_seconds) = network
                        .transfer_seconds(&previous.ride.alight_stop_id, &ride.board_stop_id)
                    else {
                        continue;
                    };
                    available_at += time::Duration::seconds(i64::from(transfer_seconds));
                }
                let condition = realtime.condition(&service.id);
                let condition_is_fresh = condition
                    .is_some_and(|condition| condition.freshness.state == FreshnessState::Fresh);
                if condition_is_fresh
                    && condition
                        .is_some_and(|condition| service_condition_blocks(condition, available_at))
                {
                    continue;
                }
                if service.service_condition_required && !condition_is_fresh {
                    continue;
                }
                let Ok(departures) = resolver
                    .next_departures(service, &ride.board_stop_id, available_at, ride, realtime)
                    .await
                else {
                    continue;
                };
                for departure in departures.into_iter().take(3) {
                    if departure.departure < available_at {
                        continue;
                    }
                    let mut evidence = Vec::new();
                    let mut required_states = Vec::new();
                    if condition_is_fresh && let Some(condition) = condition {
                        evidence.push(condition.clone());
                        required_states.push(condition.freshness.state);
                    }
                    if let Some(status) = departure.live_status {
                        required_states.push(status.freshness.state);
                        evidence.push(status);
                    }
                    let mut rides = partial.rides.clone();
                    rides.push(TimedTransitRide {
                        ride: departure.ride,
                        departure: departure.departure,
                        arrival: departure.arrival,
                        evidence,
                        required_states,
                        schedule_only: departure.schedule_only,
                    });
                    next_partials.push(Partial {
                        available_at: departure.arrival,
                        rides,
                    });
                }
            }
            next_partials.sort_by(|left, right| {
                left.available_at
                    .cmp(&right.available_at)
                    .then(timed_signature(&left.rides).cmp(&timed_signature(&right.rides)))
            });
            next_partials.dedup_by(|left, right| {
                left.available_at == right.available_at
                    && timed_signature(&left.rides) == timed_signature(&right.rides)
            });
            next_partials.truncate(8);
            partials = next_partials;
            if partials.is_empty() {
                break;
            }
        }
        for partial in partials {
            let Some(last) = partial.rides.last() else {
                continue;
            };
            if context
                .latest_arrival
                .is_none_or(|latest| last.arrival <= latest)
            {
                timed.push(TimedTransitItinerary {
                    service_day: itinerary.service_day,
                    access_departure_at: context.search_start,
                    arrival_at: last.arrival,
                    rides: partial.rides,
                });
            }
        }
    }
    timed.sort_by(|left, right| {
        left.arrival_at
            .cmp(&right.arrival_at)
            .then(left.access_departure_at.cmp(&right.access_departure_at))
            .then(timed_signature(&left.rides).cmp(&timed_signature(&right.rides)))
    });
    timed.dedup_by(|left, right| {
        left.access_departure_at == right.access_departure_at
            && timed_signature(&left.rides) == timed_signature(&right.rides)
    });
    TimedTransitBackbone { itineraries: timed }
}

/// Resolve a topology-only backbone. The topology may contain synthetic ride
/// shapes for buses, including services with no active static trip. Rail-like
/// services still pass through the schedule resolver and therefore remain
/// schedule-driven.
pub(crate) async fn resolve_topology_backbone(
    network: &TransitNetwork,
    candidate: &TransitCandidateNetwork,
    topologies: &[TransitTopologyItinerary],
    realtime: &RealtimeContext,
    context: &PlanningContext,
) -> TimedTransitBackbone {
    let itineraries = topologies
        .iter()
        .map(|topology| TransitItinerary {
            service_day: topology.service_day,
            access_departure_at: context.search_start,
            arrival_at: topology
                .rides
                .last()
                .map(|ride| ride.scheduled_arrival)
                .unwrap_or(context.search_start),
            rides: topology.rides.clone(),
        })
        .collect::<Vec<_>>();
    resolve_backbone(network, candidate, &itineraries, realtime, context).await
}

fn service_condition_blocks(status: &NormalizedRealtimeValue, at: OffsetDateTime) -> bool {
    let RealtimeValue::ServiceCondition(condition) = &status.value else {
        return false;
    };
    condition.effective_from.is_none_or(|from| from <= at)
        && condition.effective_until.is_none_or(|until| at < until)
}

fn timed_signature(rides: &[TimedTransitRide]) -> String {
    rides
        .iter()
        .map(|ride| {
            format!(
                "{}:{}:{}:{}",
                ride.ride.service_id.as_str(),
                ride.ride.trip_id.as_str(),
                ride.ride.board_stop_id.as_str(),
                ride.ride.alight_stop_id.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use time::macros::{date, datetime};

    use super::*;
    use crate::{
        api::RequestCoordinate,
        normalization::{BoardingPointId, ScheduledRunId, TransitServiceId},
        transit::{ScheduledStopTime, ScheduledTrip, TransitStop},
    };

    fn fixture(mode: TransitMode) -> (TransitNetwork, TransitService, TransitRide) {
        let board = BoardingPointId::new("board");
        let alight = BoardingPointId::new("alight");
        let service_id = TransitServiceId::new("service");
        let service = TransitService::new(
            service_id.clone(),
            mode,
            "Fixture service",
            "Alight",
            vec![board.clone(), alight.clone()],
        );
        let network = TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![
                TransitStop::new(
                    board.clone(),
                    "Board",
                    RequestCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                ),
                TransitStop::new(
                    alight.clone(),
                    "Alight",
                    RequestCoordinate {
                        latitude: 25.01,
                        longitude: 121.51,
                    },
                ),
            ],
            vec![service.clone()],
            vec![ScheduledTrip::weekdays(
                ScheduledRunId::new("run"),
                service_id.clone(),
                vec![time::Weekday::Monday],
                vec![
                    ScheduledStopTime::new(21_600, 21_600),
                    ScheduledStopTime::new(22_200, 22_200),
                ],
            )],
            Vec::new(),
        );
        let ride = network
            .scheduled_ride(
                date!(2026 - 08 - 31),
                &service_id,
                &ScheduledRunId::new("run"),
                &board,
                &alight,
            )
            .unwrap();
        (network, service, ride)
    }

    #[tokio::test]
    async fn bus_requires_realtime_while_schedule_modes_use_schedule() {
        for mode in [
            TransitMode::Bus,
            TransitMode::Metro,
            TransitMode::Rail,
            TransitMode::HighSpeedRail,
        ] {
            let (network, service, ride) = fixture(mode);
            let resolver = NetworkTransitDepartureResolver::new(&network);
            let departures = resolver
                .next_departures(
                    &service,
                    &ride.board_stop_id,
                    datetime!(2026-08-31 05:00 +08:00),
                    &ride,
                    &RealtimeContext::default(),
                )
                .await
                .unwrap();
            if mode == TransitMode::Bus {
                assert!(departures.is_empty());
            } else {
                assert_eq!(departures.len(), 1);
                assert!(departures[0].schedule_only);
            }
        }
    }
}
