//! Journey augmentation between transit search and reconstruction.

mod bike;
mod skeleton;

pub use bike::{BikeAugmentationPolicy, BikeJourneyAugmenter};
pub use skeleton::{
    BikeRide, BikeShortcut, JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
    JourneyPathMovement, JourneyPathStep, JourneySkeleton, JourneySkeletonError,
    JourneyTimingError, JourneyVariant, JourneyVariantTiming, WalkingMovementKind,
};

use async_trait::async_trait;

/// Selects one or more paths through an already-found journey skeleton.
#[async_trait]
pub trait JourneyAugmenter: Send + Sync {
    async fn augment(&self, skeleton: JourneySkeleton) -> Vec<JourneyVariant>;
}

/// The current augmenter preserves the Trip-Based baseline exactly.
#[derive(Clone, Copy, Debug, Default)]
pub struct BaselineJourneyAugmenter;

#[async_trait]
impl JourneyAugmenter for BaselineJourneyAugmenter {
    async fn augment(&self, skeleton: JourneySkeleton) -> Vec<JourneyVariant> {
        vec![JourneyVariant::baseline(skeleton)]
    }
}
