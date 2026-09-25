use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{
    transit::{TransitMode, TransitNetwork},
    transit_timing::{
        DepartureError, DepartureQuery, EffectiveTrip, ScheduleTiming, TimingReliability,
    },
};

use super::source::TransitDepartureSource;

/// Static-only THSR timing. The timetable is an authoritative, reliable source.
pub struct ThsrDepartureSource {
    network: Arc<TransitNetwork>,
    schedule: ScheduleTiming,
}

impl ThsrDepartureSource {
    pub fn new(network: Arc<TransitNetwork>) -> Self {
        Self {
            schedule: ScheduleTiming::new(network.clone()),
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
        let Some(service) = self.network.pattern(&query.pattern_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if self.network.route_of(service).mode != TransitMode::HighSpeedRail
            || !service.stops.contains(&query.board_stop_id)
        {
            return Err(DepartureError::InvalidQuery);
        }
        self.schedule.next_departures(query).await
    }
}
