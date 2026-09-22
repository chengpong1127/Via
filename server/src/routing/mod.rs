//! Provider-neutral routing pipeline.

pub(crate) mod access;
pub mod augmentation;
pub mod model;
pub mod planner;
pub mod reachability;
pub mod reconstruct;
pub mod temporal;
pub mod topology;
pub mod transfers;

pub(crate) mod service;
pub(crate) mod status;

pub use augmentation::{
    BikeAugmentationPolicy, BikeJourneyAugmenter, BikeRide, BikeShortcut, JourneyAnchor,
    JourneyAnchorKind, JourneyMovement, JourneyMovementKind, JourneyPathMovement, JourneyPathStep,
    JourneySkeleton, JourneySkeletonError, JourneyTimingError, JourneyVariant,
    JourneyVariantTiming, WalkingMovementKind,
};
pub use model::{
    AccessPoint, EgressPoint, JourneyCandidate, JourneySegment, RoutingError, TransitQuery,
};
pub use planner::{TransitPlanner, TransitPlannerStats};
pub use service::{RoutingLimits, RoutingService};
pub use transfers::{TransferShortcut, TransferShortcutIndex};

pub type RouteQuery = crate::api::PlanningRequest;
