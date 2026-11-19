use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::network::{SegmentTravelTime, TransitMode},
    transit::runtime::TransitRuntime,
};

use crate::routing::model::PlanningDeadline;

use super::types::CorridorSearchStats;

#[derive(Clone, Copy, Debug)]
pub(super) struct PatternRideCache<'a> {
    pub(super) mode: TransitMode,
    pub(super) pattern_id: &'a TransitPatternId,
    pub(super) stops: &'a [BoardingPointId],
    pub(super) segments: &'a [SegmentTravelTime],
}

#[derive(Clone, Debug, Default)]
pub(super) struct CorridorPatternIndex<'a> {
    pub(super) groups: Vec<PatternRideCache<'a>>,
    pub(super) group_index_by_pattern: HashMap<&'a TransitPatternId, usize>,
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
            let group_index = index.groups.len();
            index
                .group_index_by_pattern
                .insert(&pattern.id, group_index);
            index.groups.push(PatternRideCache {
                mode,
                pattern_id: &pattern.id,
                stops: &pattern.stops,
                segments: &pattern.static_profile.segments,
            });
        }
        index
    }

    pub(super) fn group_index(&self, pattern_id: &TransitPatternId) -> Option<usize> {
        self.group_index_by_pattern.get(pattern_id).copied()
    }

    pub(super) fn group(&self, group_index: usize) -> PatternRideCache<'a> {
        self.groups[group_index]
    }
}
