use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::network::{SegmentTravelTime, TransitMode},
    transit::runtime::TransitRuntime,
};

use crate::routing::model::PlanningDeadline;

use super::types::CorridorSearchStats;

#[derive(Clone, Copy, Debug)]
pub(super) struct IndexedPattern<'a> {
    pub(super) mode: TransitMode,
    pub(super) pattern_id: &'a TransitPatternId,
    pub(super) stops: &'a [BoardingPointId],
    pub(super) segments: &'a [SegmentTravelTime],
}

#[derive(Clone, Debug, Default)]
pub(super) struct CorridorPatternIndex<'a> {
    patterns: HashMap<&'a TransitPatternId, IndexedPattern<'a>>,
}

impl<'a> CorridorPatternIndex<'a> {
    pub(super) fn new(
        transit: &'a TransitRuntime,
        allowed_modes: &[TransitMode],
        stats: &mut CorridorSearchStats,
        deadline: PlanningDeadline,
    ) -> Self {
        let mut index = Self::default();
        for pattern in &transit.network().patterns {
            if deadline.is_elapsed() {
                stats.deadline_exceeded = true;
                break;
            }
            let mode = transit.required_route(&pattern.route_id).mode;
            if !allowed_modes.contains(&mode) {
                continue;
            }
            index.patterns.insert(
                &pattern.id,
                IndexedPattern {
                    mode,
                    pattern_id: &pattern.id,
                    stops: &pattern.stops,
                    segments: &pattern.static_profile.segments,
                },
            );
        }
        index
    }

    pub(super) fn get(&self, pattern_id: &TransitPatternId) -> Option<IndexedPattern<'a>> {
        self.patterns.get(pattern_id).copied()
    }
}
