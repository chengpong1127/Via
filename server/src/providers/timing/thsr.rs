use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{
    transit::network::TransitMode,
    transit::runtime::TransitRuntime,
    transit::timing::{
        DepartureError, DepartureQuery, EffectiveTrip, ScheduleTiming, TimingReliability,
    },
};

use super::source::TransitDepartureSource;

pub struct ThsrDepartureSource {
    transit: Arc<TransitRuntime>,
    schedule: ScheduleTiming,
}

impl ThsrDepartureSource {
    pub fn new(transit: Arc<TransitRuntime>) -> Self {
        Self {
            schedule: ScheduleTiming::new(transit.clone()),
            transit,
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
        query.validate_for(&self.transit, TransitMode::HighSpeedRail)?;
        self.schedule.next_departures(query).await
    }
}
