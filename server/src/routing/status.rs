//! Request-scoped realtime facts that remain useful after routing.

use std::collections::{HashMap, HashSet};

use crate::{
    domain::{
        ids::{BikeShareStationId, TransitServiceId},
        realtime::{NormalizedRealtimeValue, RealtimeValue},
    },
    providers::realtime::RealtimeProvider,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct RealtimeContext {
    bikes: HashMap<BikeShareStationId, NormalizedRealtimeValue>,
}

impl RealtimeContext {
    pub(crate) fn from_values(values: impl IntoIterator<Item = NormalizedRealtimeValue>) -> Self {
        let mut context = Self::default();
        for value in values {
            if let RealtimeValue::BikeAvailability(availability) = &value.value {
                context.bikes.insert(availability.station_id.clone(), value);
            }
        }
        context
    }

    pub(crate) fn bike(&self, station_id: &BikeShareStationId) -> Option<&NormalizedRealtimeValue> {
        self.bikes.get(station_id)
    }
}

pub(crate) async fn collect_service_conditions(
    realtime: &dyn RealtimeProvider,
    service_ids: impl IntoIterator<Item = TransitServiceId>,
) -> (Vec<NormalizedRealtimeValue>, bool) {
    let mut service_ids = service_ids.into_iter().collect::<Vec<_>>();
    service_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    service_ids.dedup();
    if service_ids.is_empty() {
        return (Vec::new(), false);
    }
    match realtime.service_conditions(&service_ids).await {
        Ok(values) => (values, false),
        Err(error) => {
            tracing::warn!(error = %error, "realtime service-condition lookup failed");
            (Vec::new(), error.is_transient())
        }
    }
}

pub(crate) async fn collect_bike_values(
    realtime: &dyn RealtimeProvider,
    station_ids: impl IntoIterator<Item = BikeShareStationId>,
) -> (RealtimeContext, bool) {
    let mut station_ids = station_ids.into_iter().collect::<Vec<_>>();
    station_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    station_ids.dedup();
    if station_ids.is_empty() {
        return (RealtimeContext::default(), false);
    }
    match realtime.bike_availability(&station_ids).await {
        Ok(values) => (RealtimeContext::from_values(values), false),
        Err(error) => {
            tracing::warn!(error = %error, "realtime BikeShare lookup failed");
            (RealtimeContext::default(), error.is_transient())
        }
    }
}

pub(crate) fn required_station_ids(
    candidates: &[crate::routing::algorithm::JourneyCandidate],
) -> HashSet<BikeShareStationId> {
    candidates
        .iter()
        .flat_map(|candidate| {
            candidate
                .access
                .required_station_ids()
                .into_iter()
                .chain(candidate.egress.required_station_ids())
                .chain(candidate.segments.iter().flat_map(|segment| match segment {
                    crate::routing::algorithm::JourneySegment::BikeShareTransfer { transfer } => {
                        transfer.required_station_ids()
                    }
                    _ => Vec::new(),
                }))
        })
        .collect()
}
