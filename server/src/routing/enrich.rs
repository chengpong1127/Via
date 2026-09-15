//! Request-scoped realtime facts and reconstruction timing adapters.
//!
//! Transit departure selection belongs to `DepartureResolver` and is executed
//! by the routing algorithm. This module retains normalized evidence and the
//! small representation consumed by detailed reconstruction.

use std::collections::{HashMap, HashSet};

use time::{Date, OffsetDateTime};

use crate::{
    domain::transit::TransitRide,
    domain::{
        ids::{BikeShareStationId, TransitServiceId},
        realtime::{FreshnessState, NormalizedRealtimeValue, RealtimeValue},
        transit_timing::TimingBasis,
    },
    providers::realtime::RealtimeProvider,
};

use super::discover::DiscoveryResult;

#[derive(Clone, Debug, Default)]
pub struct RealtimeContext {
    bikes: HashMap<BikeShareStationId, NormalizedRealtimeValue>,
    conditions: HashMap<TransitServiceId, NormalizedRealtimeValue>,
}

pub(crate) async fn collect_request_realtime(
    realtime: &dyn RealtimeProvider,
    candidate: &DiscoveryResult,
    itineraries: &[crate::domain::transit::TransitItinerary],
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
}
