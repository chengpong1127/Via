//! Via's provider-neutral routing boundary.

pub mod engine;

pub mod bike {
    pub use super::engine::bike_share_expansion::*;
}

pub mod discover {
    pub use super::engine::candidate_discovery::*;
}

pub mod enrich {
    pub use super::engine::transit_timing::*;
}

pub mod graph {
    pub use super::engine::multimodal_graph::*;
}

pub mod search {
    pub use super::engine::search::*;
}

pub use crate::domain::ids::IdentityRepository;
pub use crate::realtime::RealtimeProvider;
pub use crate::street::StreetRoutingProvider;
pub use engine::{JourneyPlanningService, PlanningError, PlanningLimits, PlanningOutcome};

/// Provider-neutral input to the routing engine.
pub type RouteQuery = crate::api::PlanningRequest;
pub type RoutingError = PlanningError;

/// A routing result intentionally omits HTTP metadata and empty-result
/// envelopes. The API adapter adds those concerns at the application edge.
pub type JourneyOption = engine::JourneyOption;

#[derive(Clone)]
pub struct RoutingService {
    planner: JourneyPlanningService,
}

impl RoutingService {
    pub fn new(planner: JourneyPlanningService) -> Self {
        Self { planner }
    }

    pub async fn route(&self, query: RouteQuery) -> Result<Vec<JourneyOption>, PlanningError> {
        match self.plan(query).await? {
            engine::PlanningOutcome::Options(result) => Ok(result.journey_options),
            engine::PlanningOutcome::Empty(_) => Ok(Vec::new()),
        }
    }

    /// Full result used by the HTTP adapter, including the provider-neutral
    /// metadata and successful empty-result envelope.
    pub async fn plan(&self, query: RouteQuery) -> Result<engine::PlanningOutcome, PlanningError> {
        self.planner.plan(query).await
    }

    pub fn planner(&self) -> &JourneyPlanningService {
        &self.planner
    }
}
