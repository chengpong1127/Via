use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::domain::transit_runtime::TransitRuntime;

#[async_trait]
pub trait DependencyHealth: Send + Sync {
    async fn healthy(&self) -> bool;

    fn name(&self) -> &'static str;
}

#[derive(Clone)]
pub struct Health {
    street: Arc<dyn DependencyHealth>,
    transit: Arc<TransitRuntime>,
}

impl Health {
    pub(crate) fn new(street: Arc<dyn DependencyHealth>, transit: Arc<TransitRuntime>) -> Self {
        Self { street, transit }
    }

    pub async fn readiness(&self) -> ReadinessReport {
        let street_routing = self.street.healthy().await;
        let revisions = self.transit.revisions();
        let static_network = !revisions.0.trim().is_empty() && !revisions.1.trim().is_empty();
        let transit_artifact = self.transit.validate().is_ok();
        ReadinessReport {
            ready: street_routing && static_network && transit_artifact,
            street_routing,
            static_network,
            transit_artifact,
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
