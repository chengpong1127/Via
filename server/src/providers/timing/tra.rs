use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{
    transit::{TransitMode, TransitNetwork},
    transit_timing::{
        DepartureError, DepartureQuery, DepartureResolver, EffectiveTrip,
        ScheduleDepartureResolver, TimingReliability,
    },
};

use super::source::TransitDepartureSource;

/// Scheduled TRA timing. A future TRA realtime implementation belongs here.
pub struct TraDepartureSource {
    network: Arc<TransitNetwork>,
    schedule: ScheduleDepartureResolver,
}

impl TraDepartureSource {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        Self {
            schedule: ScheduleDepartureResolver::new(network.clone()),
            network,
        }
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
        let Some(service) = self.network.service(&query.service_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if service.transit_mode != TransitMode::Rail
            || !service.pattern.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        self.schedule.next_departures(query).await
    }
}
