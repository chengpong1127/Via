//! Provider-neutral routing pipeline.

pub mod bike;
pub mod discover;
pub mod enrich;
pub mod graph;
pub mod reconstruct;
pub mod search;

pub(crate) mod service;

pub use crate::domain::journey::{JourneyOption, PlanningError, PlanningOutcome};
pub use crate::providers::realtime::RealtimeProvider;
pub use crate::street::StreetRoutingProvider;
pub use graph::StreetMetric;
pub use service::{RoutingLimits, RoutingService};

pub type RouteQuery = crate::api::PlanningRequest;
