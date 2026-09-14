//! Phase 2: resolve departures for the discovered transit topology.
//! Timing source selection is owned by `TransitTimingProvider`; this module
//! only turns normalized departures into timed graph-ready rides.

use std::collections::{HashMap, HashSet};

use futures_util::{StreamExt, stream};
use time::{Date, OffsetDateTime};

use crate::{
    domain::transit::{TransitItinerary, TransitNetwork, TransitRide, TransitTopologyItinerary},
    domain::{
        ids::{BikeShareStationId, TransitServiceId},
        realtime::{FreshnessState, NormalizedRealtimeValue, RealtimeValue},
        transit_timing::{TimingBasis, TransitTimingProvider, TransitTimingQuery},
    },
    providers::realtime::RealtimeProvider,
};

use super::{discover::DiscoveryResult, service::PlanningContext};

#[derive(Clone, Debug, Default)]
pub struct RealtimeContext {
    bikes: HashMap<BikeShareStationId, NormalizedRealtimeValue>,
    conditions: HashMap<TransitServiceId, NormalizedRealtimeValue>,
}

pub(crate) async fn collect_request_realtime(
    realtime: &dyn RealtimeProvider,
    candidate: &DiscoveryResult,
    itineraries: &[TransitItinerary],
    max_itineraries: usize,
) -> (RealtimeContext, bool) {
    let mut service_ids = HashSet::new();
    for itinerary in itineraries.iter().take(max_itineraries) {
        for ride in &itinerary.rides {
            service_ids.insert(ride.service_id.clone());
        }
    }
    for itinerary in candidate.topology_itineraries.iter().take(max_itineraries) {
        for ride in &itinerary.rides {
            service_ids.insert(ride.service_id.clone());
        }
    }

    let mut values = Vec::new();
    let mut had_transient_failure = false;
    if !service_ids.is_empty() {
        let mut ids = service_ids.into_iter().collect::<Vec<_>>();
        ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        match realtime.service_conditions(&ids).await {
            Ok(conditions) => values.extend(conditions),
            Err(error) => {
                had_transient_failure |= error.is_transient();
                tracing::warn!(error = %error, "realtime service-condition enrichment failed");
            }
        }
    }
    (RealtimeContext::from_values(values), had_transient_failure)
}

pub(crate) async fn collect_bike_realtime(
    realtime: &dyn RealtimeProvider,
    station_ids: &[BikeShareStationId],
) -> (Vec<NormalizedRealtimeValue>, bool) {
    if station_ids.is_empty() {
        return (Vec::new(), false);
    }
    match realtime.bike_availability(station_ids).await {
        Ok(values) => (values, false),
        Err(error) => {
            tracing::warn!(error = %error, "realtime BikeShare enrichment failed");
            (Vec::new(), error.is_transient())
        }
    }
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
                RealtimeValue::ArrivalEstimate(_) => {}
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
    }

    pub fn bike(&self, station_id: &BikeShareStationId) -> Option<&NormalizedRealtimeValue> {
        self.bikes.get(station_id)
    }

    pub fn station_ids(&self) -> impl Iterator<Item = &BikeShareStationId> {
        self.bikes.keys()
    }

    pub fn condition(&self, service_id: &TransitServiceId) -> Option<&NormalizedRealtimeValue> {
        self.conditions.get(service_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimedTransitRide {
    pub ride: TransitRide,
    pub departure: OffsetDateTime,
    pub arrival: OffsetDateTime,
    pub evidence: Vec<NormalizedRealtimeValue>,
    pub required_states: Vec<FreshnessState>,
    pub timing_basis: TimingBasis,
}

impl TimedTransitRide {
    pub fn is_schedule_only(&self) -> bool {
        matches!(self.timing_basis, TimingBasis::Schedule)
    }
}

#[derive(Clone, Debug)]
pub struct TimedTransitItinerary {
    pub service_day: Date,
    pub access_departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub rides: Vec<TimedTransitRide>,
}

#[derive(Clone, Debug, Default)]
pub struct TransitTiming {
    pub itineraries: Vec<TimedTransitItinerary>,
    pub(crate) had_transient_failure: bool,
    pub(crate) had_global_failure: bool,
}

pub(crate) async fn resolve_timing(
    network: &TransitNetwork,
    candidate: &DiscoveryResult,
    static_itineraries: &[crate::domain::transit::TransitItinerary],
    timing_provider: &dyn TransitTimingProvider,
    realtime: &RealtimeContext,
    context: &PlanningContext,
    max_concurrency: usize,
) -> TransitTiming {
    let mut timed = Vec::new();
    let mut had_transient_failure = false;
    let mut had_global_failure = false;
    for itinerary in static_itineraries {
        let Some(first) = itinerary.rides.first() else {
            continue;
        };
        let Some(_access) = candidate.access_for(&first.board_stop_id) else {
            continue;
        };
        // Keep the static timing pool broad enough to include a departure that a
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
            let mut requests = Vec::new();
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
                requests.push((
                    partial,
                    ride.clone(),
                    available_at,
                    service.timing_source.clone(),
                    condition.cloned(),
                    condition_is_fresh,
                ));
            }
            let mut resolved =
                stream::iter(requests.into_iter().map(
                    |(
                        partial,
                        ride,
                        available_at,
                        timing_source,
                        condition,
                        condition_is_fresh,
                    )| async move {
                        let result = timing_provider
                            .next_departures(TransitTimingQuery {
                                service_id: ride.service_id.clone(),
                                board_stop_id: ride.board_stop_id.clone(),
                                alight_stop_id: ride.alight_stop_id.clone(),
                                timing_source,
                                after: available_at,
                                limit: 3,
                                scheduled_ride: ride.clone(),
                            })
                            .await;
                        (
                            partial,
                            ride,
                            available_at,
                            condition,
                            condition_is_fresh,
                            result,
                        )
                    },
                ))
                .buffer_unordered(max_concurrency.max(1))
                .collect::<Vec<_>>()
                .await;
            resolved.sort_by(|left, right| {
                timed_signature(&left.0.rides).cmp(&timed_signature(&right.0.rides))
            });
            let mut next_partials = Vec::new();
            for (partial, ride, available_at, condition, condition_is_fresh, result) in resolved {
                let departures = match result {
                    Ok(departures) => departures,
                    Err(error) => {
                        had_transient_failure |= error.is_transient();
                        had_global_failure |= error.is_global();
                        tracing::warn!(
                            service_id = ride.service_id.as_str(),
                            board_stop_id = ride.board_stop_id.as_str(),
                            error = %error,
                            "transit timing resolution failed"
                        );
                        continue;
                    }
                };
                for departure in departures.into_iter().take(3) {
                    if departure.departure_at < available_at
                        || departure.arrival_at < departure.departure_at
                    {
                        continue;
                    }
                    let mut evidence = Vec::new();
                    let mut required_states = Vec::new();
                    if condition_is_fresh && let Some(condition) = &condition {
                        evidence.push(condition.clone());
                        required_states.push(condition.freshness.state);
                    }
                    required_states.extend(
                        departure
                            .evidence
                            .iter()
                            .map(|status| status.freshness.state),
                    );
                    evidence.extend(departure.evidence);
                    let mut timed_ride = ride.clone();
                    if let Some(trip_id) = departure.trip_id {
                        timed_ride.trip_id = trip_id;
                    }
                    let mut rides = partial.rides.clone();
                    rides.push(TimedTransitRide {
                        ride: timed_ride,
                        departure: departure.departure_at,
                        arrival: departure.arrival_at,
                        evidence,
                        required_states,
                        timing_basis: departure.timing_basis,
                    });
                    next_partials.push(Partial {
                        available_at: departure.arrival_at,
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
    TransitTiming {
        itineraries: timed,
        had_transient_failure,
        had_global_failure,
    }
}

/// Resolve timing for topology-only discovery. The topology may contain
/// synthetic ride shapes for services without an active static trip.
pub(crate) async fn resolve_topology_timing(
    network: &TransitNetwork,
    candidate: &DiscoveryResult,
    topologies: &[TransitTopologyItinerary],
    timing_provider: &dyn TransitTimingProvider,
    realtime: &RealtimeContext,
    context: &PlanningContext,
    max_concurrency: usize,
) -> TransitTiming {
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
    resolve_timing(
        network,
        candidate,
        &itineraries,
        timing_provider,
        realtime,
        context,
        max_concurrency,
    )
    .await
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
                "{}:{}:{}:{}:{}:{}",
                ride.ride.service_id.as_str(),
                ride.ride.trip_id.as_str(),
                ride.ride.board_stop_id.as_str(),
                ride.ride.alight_stop_id.as_str(),
                ride.departure.unix_timestamp_nanos(),
                ride.arrival.unix_timestamp_nanos()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}
