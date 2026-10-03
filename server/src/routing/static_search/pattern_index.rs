use std::collections::HashMap;

use crate::domain::{
    ids::{BoardingPointId, TransitPatternId},
    transit::{SegmentTimeSource, SegmentTravelTime, TransitMode},
    transit_runtime::TransitRuntime,
};

use crate::routing::model::PlanningDeadline;

use super::types::{EquivalentLegKey, StaticCostSource, StaticSearchStats, StaticTransitLeg};

#[derive(Clone, Debug)]
pub(super) struct PatternRideOption {
    pub(super) alight_stop_id: BoardingPointId,
    pub(super) leg: StaticTransitLeg,
    pub(super) duration_seconds: u32,
}

#[derive(Clone, Debug)]
pub(super) struct PatternRideCache {
    pub(super) mode: TransitMode,
    pub(super) pattern_id: TransitPatternId,
    pub(super) stops: Vec<BoardingPointId>,
    pub(super) segments: Vec<SegmentTravelTime>,
    pub(super) rides_by_board_index: HashMap<usize, Vec<PatternRideOption>>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct StaticPhase1PatternIndex {
    pub(super) groups: Vec<PatternRideCache>,
    pub(super) group_index_by_pattern: HashMap<TransitPatternId, usize>,
}

impl StaticPhase1PatternIndex {
    pub(super) fn new(
        network: &TransitRuntime,
        allowed_modes: &[TransitMode],
        stats: &mut StaticSearchStats,
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
            stats.static_pattern_index_pattern_edges_scanned +=
                pattern.static_profile.segments.len();
            stats.static_pattern_index_positions_scanned += pattern.static_profile.segments.len();
            index.groups.push(PatternRideCache {
                mode,
                pattern_id: pattern.id.clone(),
                stops: pattern.stops.clone(),
                segments: pattern.static_profile.segments.clone(),
                rides_by_board_index: HashMap::new(),
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

    pub(super) fn ensure_ride_options(
        &mut self,
        group_index: usize,
        board_index: usize,
        stats: &mut StaticSearchStats,
        equivalent_legs: &mut HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
        deadline: PlanningDeadline,
    ) -> bool {
        let group = &mut self.groups[group_index];
        if group.rides_by_board_index.contains_key(&board_index) {
            return false;
        }
        let mut ride_options = Vec::new();
        {
            let pattern_id = &group.pattern_id;
            stats.static_ride_profile_patterns_scanned += 1;
            let mut duration_seconds = 0_u32;
            let mut first_source = None;
            let mut mixed_source = false;
            for alight_index in (board_index + 1)..group.stops.len() {
                if deadline.is_elapsed() {
                    stats.deadline_exceeded = true;
                    return false;
                }
                stats.static_ride_profile_positions_scanned += 1;
                let segment = &group.segments[alight_index - 1];
                let Some(total) = duration_seconds.checked_add(segment.seconds) else {
                    break;
                };
                duration_seconds = total;
                mixed_source |= first_source.is_some_and(|first| first != segment.source);
                first_source.get_or_insert(segment.source);
                let cost_source = if mixed_source {
                    StaticCostSource::Mixed
                } else {
                    match segment.source {
                        SegmentTimeSource::Schedule => StaticCostSource::ExactSchedule,
                        SegmentTimeSource::Derived => StaticCostSource::DerivedBus,
                        SegmentTimeSource::Estimated => StaticCostSource::OptimisticEstimate,
                    }
                };
                let alight_stop_id = group.stops[alight_index].clone();
                let leg = StaticTransitLeg {
                    pattern_id: pattern_id.clone(),
                    board_stop_id: group.stops[board_index].clone(),
                    board_index,
                    alight_stop_id,
                    alight_index,
                    cost_source,
                };
                let key = EquivalentLegKey {
                    mode: group.mode,
                    board_stop_id: leg.board_stop_id.clone(),
                    alight_stop_id: leg.alight_stop_id.clone(),
                };
                let legs_for_key = equivalent_legs.entry(key).or_default();
                if !legs_for_key.iter().any(|known| {
                    known.pattern_id == leg.pattern_id
                        && known.board_index == leg.board_index
                        && known.alight_index == leg.alight_index
                }) {
                    legs_for_key.push(leg.clone());
                }
                ride_options.push(PatternRideOption {
                    alight_stop_id: leg.alight_stop_id.clone(),
                    leg,
                    duration_seconds,
                });
            }
        }
        group.rides_by_board_index.insert(board_index, ride_options);
        true
    }

    pub(super) fn ride_options(
        &self,
        group_index: usize,
        board_index: usize,
    ) -> &[PatternRideOption] {
        self.groups[group_index]
            .rides_by_board_index
            .get(&board_index)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
