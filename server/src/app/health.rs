use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::{domain::transit::runtime::TransitRuntime, street::DependencyHealth};

#[derive(Clone)]
pub struct Health {
    street: Arc<dyn DependencyHealth>,
    static_network: bool,
    transit_artifact: bool,
}

impl Health {
    pub(crate) fn new(street: Arc<dyn DependencyHealth>, transit: &TransitRuntime) -> Self {
        Self {
            street,
            static_network: transit.has_revisions(),
            transit_artifact: transit.validate().is_ok(),
        }
    }

    pub async fn readiness(&self) -> ReadinessReport {
        let street_routing = self.street.healthy().await;
        ReadinessReport {
            ready: street_routing && self.static_network && self.transit_artifact,
            street_routing,
            static_network: self.static_network,
            transit_artifact: self.transit_artifact,
        }
    }

    pub async fn street_routing(&self) -> DependencyHealthReport {
        DependencyHealthReport {
            healthy: self.street.healthy().await,
            provider: self.street.name().to_owned(),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct ReadinessReport {
    pub ready: bool,
    pub street_routing: bool,
    pub static_network: bool,
    pub transit_artifact: bool,
}

#[derive(Serialize, ToSchema)]
pub struct DependencyHealthReport {
    pub healthy: bool,
    pub provider: String,
}
