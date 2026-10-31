mod pattern_index;
mod retention;
mod search;
#[cfg(test)]
mod tests;
mod types;

pub use retention::RetainedCorridorSet;
pub use search::generate;
pub use types::{
    StaticCorridor, StaticCorridorStep, StaticCostSource, StaticSearchStats, StaticTransitLeg,
};
