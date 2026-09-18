//! Request-scoped transit realtime facts that remain useful after routing.

use crate::{
    domain::{ids::TransitServiceId, realtime::NormalizedRealtimeValue},
    providers::realtime::RealtimeProvider,
};

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
