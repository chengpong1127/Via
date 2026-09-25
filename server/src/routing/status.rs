//! Request-scoped transit realtime facts that remain useful after routing.

use crate::{
    domain::{ids::TransitPatternId, realtime::NormalizedRealtimeValue},
    providers::service_conditions::ServiceConditionProvider,
};

pub(crate) async fn collect_service_conditions(
    service_conditions: &dyn ServiceConditionProvider,
    service_ids: impl IntoIterator<Item = TransitPatternId>,
) -> (Vec<NormalizedRealtimeValue>, bool) {
    let mut service_ids = service_ids.into_iter().collect::<Vec<_>>();
    service_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    service_ids.dedup();
    if service_ids.is_empty() {
        return (Vec::new(), false);
    }
    match service_conditions.service_conditions(&service_ids).await {
        Ok(values) => (values, false),
        Err(error) => {
            tracing::warn!(
                target: "via_server::providers::tdx",
                provider = "tdx",
                operation = "service_conditions",
                outcome = if error.is_transient() { "unavailable" } else { "rejected" },
                error = %error,
                "service-condition lookup failed"
            );
            (Vec::new(), error.is_transient())
        }
    }
}
