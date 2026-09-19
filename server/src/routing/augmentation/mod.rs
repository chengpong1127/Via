//! Journey augmentation between transit search and reconstruction.

mod bike;
mod skeleton;

pub use bike::{BikeAugmentationPolicy, BikeJourneyAugmenter};
pub use skeleton::{
    BikeRide, BikeShortcut, JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
    JourneyPathMovement, JourneyPathStep, JourneySkeleton, JourneySkeletonError,
    JourneyTimingError, JourneyVariant, JourneyVariantTiming, WalkingMovementKind,
};
