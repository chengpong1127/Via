use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::{SegmentTravelTime, TransitMode},
    transit_runtime::TransitRuntime,
};

use crate::routing::model::PlanningDeadline;

use super::types::CorridorSearchStats;

#[derive(Clone, Debug)]
pub(super) struct PatternRideCache {
    pub(super) mode: TransitMode,
    pub(super) pattern_id: TransitPatternId,
    pub(super) stops: Vec<BoardingPointId>,
    pub(super) segments: Vec<SegmentTravelTime>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CorridorPatternIndex {
    pub(super) groups: Vec<PatternRideCache>,
    pub(super) group_index_by_pattern: HashMap<TransitPatternId, usize>,
}

impl CorridorPatternIndex {
    pub(super) fn new(
        network: &TransitRuntime,
        allowed_modes: &[TransitMode],
        stats: &mut CorridorSearchStats,
        deadline: PlanningDeadline,
    ) -> Self {
        let mut index = Self::default();
        for pattern in &network.network().patterns {
            if deadline.is_elapsed() {
                stats.deadline_exceeded = true;
                break;
            }
            let mode = network.route_of(pattern).mode;
            if !allowed_modes.contains(&mode) {
                continue;
            }
            let group_index = index.groups.len();
            index
                .group_index_by_pattern
                .insert(pattern.id.clone(), group_index);
            index.groups.push(PatternRideCache {
                mode,
                pattern_id: pattern.id.clone(),
                stops: pattern.stops.clone(),
                segments: pattern.static_profile.segments.clone(),
            });
        }
        index
    }

    pub(super) fn group_index(&self, pattern_id: &TransitPatternId) -> Option<usize> {
        self.group_index_by_pattern.get(pattern_id).copied()
    }

    pub(super) fn group(&self, group_index: usize) -> &PatternRideCache {
        &self.groups[group_index]
    }
}
