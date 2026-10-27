pub(crate) mod access;
mod bike;
mod corridor;
mod evaluator;
mod matrix;
#[cfg(test)]
mod matrix_tests;
pub mod model;
pub mod planner;
mod realized;
mod reconstruct;
mod request_cache;
pub mod static_search;
pub mod topology;
pub mod transfers;

pub(crate) mod service;
pub use evaluator::Phase2EvaluationStats;
pub use model::JourneyPlanningLimits;
pub use model::{
    AccessPoint, EgressPoint, PlannerLimits, PlanningDeadline, PlanningQuery, RoutingError,
};
pub use planner::{MultimodalPlanner, MultimodalPlannerStats};
pub use realized::{
    BikeRide, BikeShareMovement, JourneyAnchor, JourneyAnchorKind, JourneyMovement,
    JourneyMovementKind, WalkingMovementKind,
};
pub use realized::{JourneyTiming, JourneyTimingError, RealizedJourney, RealizedJourneyError};
pub use service::RoutingService;

pub use bike::BikeMovementPolicy;

pub use matrix::MatrixResolverStats;
