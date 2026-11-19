mod corridor;
mod retention;
mod search;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use corridor::CorridorAnchorKey;
pub use corridor::{Corridor, CorridorAnchor, PlannedMovement};
pub use retention::RetainedCorridorSet;
pub use search::generate;
pub use types::{CorridorSearchStats, StaticCorridorStep, StaticCostSource, StaticTransitLeg};
