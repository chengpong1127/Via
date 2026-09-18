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

/// Static-only THSR timing. The timetable is an authoritative, reliable source.
pub struct ThsrDepartureSource {
    network: Arc<TransitNetwork>,
    schedule: ScheduleDepartureResolver,
}

impl ThsrDepartureSource {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        Self {
            schedule: ScheduleDepartureResolver::new(network.clone()),
            network,
        }
    }
}

#[async_trait]
impl TransitDepartureSource for ThsrDepartureSource {
    fn mode(&self) -> TransitMode {
        TransitMode::HighSpeedRail
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
        if service.transit_mode != TransitMode::HighSpeedRail
            || !service.pattern.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        self.schedule.next_departures(query).await
    }
}
