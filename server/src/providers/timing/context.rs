use std::sync::Arc;

use super::realtime_check::RealtimeCheck;
use crate::{
    domain::transit::{runtime::TransitRuntime, timing::ScheduleTiming},
    live::LiveDataSource,
    providers::tdx_bindings::TdxProviderBindings,
    tdx::RealtimeSettings,
};

#[derive(Clone)]
pub struct TdxTimingContext {
    pub(super) transit: Arc<TransitRuntime>,
    pub(super) schedule: ScheduleTiming,
    pub(super) live: Arc<dyn LiveDataSource>,
    pub(super) realtime_check: RealtimeCheck,
    pub(super) bindings: Arc<TdxProviderBindings>,
}

impl TdxTimingContext {
    pub fn new(
        transit: Arc<TransitRuntime>,
        live: Arc<dyn LiveDataSource>,
        realtime: &RealtimeSettings,
        bindings: Arc<TdxProviderBindings>,
    ) -> Self {
        Self {
            schedule: ScheduleTiming::new(transit.clone()),
            transit,
            live,
            realtime_check: RealtimeCheck::new(realtime),
            bindings,
        }
    }
}
