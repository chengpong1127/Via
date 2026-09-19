//! Provider-neutral routing pipeline.

pub(crate) mod access;
pub mod algorithm;
pub mod augmentation;
pub mod reconstruct;
pub mod transfers;
pub mod trip_based;

pub(crate) mod service;
pub(crate) mod status;

pub use algorithm::{
    AccessPoint, AlgorithmQuery, EgressPoint, JourneyCandidate, JourneySegment, RoutingAlgorithm,
    RoutingError,
};
pub use augmentation::{
    BikeAugmentationPolicy, BikeJourneyAugmenter, BikeRide, BikeShortcut, JourneyAnchor,
    JourneyAnchorKind, JourneyMovement, JourneyMovementKind, JourneyPathMovement, JourneyPathStep,
    JourneySkeleton, JourneySkeletonError, JourneyTimingError, JourneyVariant,
    JourneyVariantTiming, WalkingMovementKind,
};
pub use service::{RoutingLimits, RoutingService};
pub use transfers::{
    ResolvedTransfer, TransferIndex, TransferNeed, TransferResolver, TransferShortcut,
    TransferShortcutIndex, TransferTarget,
};

pub type RouteQuery = crate::api::PlanningRequest;
