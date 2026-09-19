//! Journey augmentation between transit search and reconstruction.

mod skeleton;

pub use skeleton::{
    JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind, JourneyPathMovement,
    JourneyPathStep, JourneySkeleton, JourneySkeletonError, JourneyVariant, WalkingMovementKind,
};

/// Selects one or more paths through an already-found journey skeleton.
pub trait JourneyAugmenter: Send + Sync {
    fn augment(&self, skeleton: JourneySkeleton) -> Vec<JourneyVariant>;
}

/// The current augmenter preserves the Trip-Based baseline exactly.
#[derive(Clone, Copy, Debug, Default)]
pub struct BaselineJourneyAugmenter;

impl JourneyAugmenter for BaselineJourneyAugmenter {
    fn augment(&self, skeleton: JourneySkeleton) -> Vec<JourneyVariant> {
        vec![JourneyVariant::baseline(skeleton)]
    }
}
