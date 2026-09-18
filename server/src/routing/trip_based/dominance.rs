use super::super::{JourneyCandidate, RoutingError};
use super::state::{BoardingNeed, DynamicLabel};
use crate::domain::ids::{BoardingPointId, TransitServiceId};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BoardingNeedKey {
    stop_id: BoardingPointId,
    round: usize,
    previous_service: Option<TransitServiceId>,
    excluded_service: Option<TransitServiceId>,
}

pub(super) fn pareto_boarding_needs(needs: Vec<BoardingNeed>) -> Vec<BoardingNeed> {
    let mut groups = HashMap::<BoardingNeedKey, Vec<BoardingNeed>>::new();
    for need in needs {
        let key = BoardingNeedKey {
            stop_id: need.stop_id.clone(),
            round: need.round,
            previous_service: need.previous_service.clone(),
            excluded_service: need.excluded_service.clone(),
        };
        groups.entry(key).or_default().push(need);
    }
    let mut pareto = groups
        .into_values()
        .flat_map(pareto_boarding_need_group)
        .collect::<Vec<_>>();
    pareto.sort_by(|left, right| {
        left.stop_id
            .as_str()
            .cmp(right.stop_id.as_str())
            .then(left.round.cmp(&right.round))
            .then(left.ready_at.cmp(&right.ready_at))
            .then(left.path.walking_seconds.cmp(&right.path.walking_seconds))
            .then(left.path.transfer_count.cmp(&right.path.transfer_count))
    });
    pareto
}

fn pareto_boarding_need_group(mut needs: Vec<BoardingNeed>) -> Vec<BoardingNeed> {
    needs.sort_by(|left, right| {
        left.ready_at
            .cmp(&right.ready_at)
            .then(left.path.walking_seconds.cmp(&right.path.walking_seconds))
            .then(left.path.transfer_count.cmp(&right.path.transfer_count))
    });
    let mut pareto = Vec::with_capacity(needs.len());
    for need in needs {
        if pareto
            .iter()
            .any(|existing| boarding_need_dominates(existing, &need))
        {
            continue;
        }
        pareto.retain(|existing| !boarding_need_dominates(&need, existing));
        pareto.push(need);
    }
    pareto
}

fn boarding_need_dominates(left: &BoardingNeed, right: &BoardingNeed) -> bool {
    left.stop_id == right.stop_id
        && left.round == right.round
        && left.previous_service == right.previous_service
        && left.excluded_service == right.excluded_service
        && left.ready_at <= right.ready_at
        && left.path.walking_seconds <= right.path.walking_seconds
        && left.path.transfer_count <= right.path.transfer_count
        && (left.ready_at < right.ready_at
            || left.path.walking_seconds < right.path.walking_seconds
            || left.path.transfer_count < right.path.transfer_count)
}

pub(super) fn insert_dynamic_label(
    labels: &mut Vec<DynamicLabel>,
    label: DynamicLabel,
) -> Option<usize> {
    if labels
        .iter()
        .any(|existing| dynamic_label_dominates_or_equals(existing, &label))
    {
        return None;
    }
    let old_len = labels.len();
    labels.retain(|existing| !dynamic_label_dominates(&label, existing));
    labels.push(label);
    Some(old_len.saturating_sub(labels.len().saturating_sub(1)))
}

pub(super) fn dynamic_label_dominates_or_equals(left: &DynamicLabel, right: &DynamicLabel) -> bool {
    left.ready_at <= right.ready_at
        && left.transfer_count <= right.transfer_count
        && left.walking_seconds <= right.walking_seconds
}

pub(super) fn dynamic_label_dominates(left: &DynamicLabel, right: &DynamicLabel) -> bool {
    dynamic_label_dominates_or_equals(left, right)
        && (left.ready_at < right.ready_at
            || left.transfer_count < right.transfer_count
            || left.walking_seconds < right.walking_seconds)
}

pub(super) fn pareto_candidates(
    mut candidates: Vec<JourneyCandidate>,
    limit: usize,
) -> Result<Vec<JourneyCandidate>, RoutingError> {
    candidates.sort_by(|left, right| {
        left.arrival_time
            .cmp(&right.arrival_time)
            .then(left.transfer_count.cmp(&right.transfer_count))
            .then(left.walking_seconds.cmp(&right.walking_seconds))
            .then(left.signature().cmp(&right.signature()))
    });
    let mut pareto = Vec::new();
    for candidate in candidates {
        if pareto.iter().any(|existing: &JourneyCandidate| {
            existing.dominates(&candidate) || existing.signature() == candidate.signature()
        }) {
            continue;
        }
        pareto.retain(|existing| !candidate.dominates(existing));
        pareto.push(candidate);
    }
    pareto.sort_by(|left, right| {
        left.arrival_time
            .cmp(&right.arrival_time)
            .then(left.transfer_count.cmp(&right.transfer_count))
            .then(left.signature().cmp(&right.signature()))
    });
    pareto.truncate(limit.max(1));
    Ok(pareto)
}

#[cfg(test)]
mod tests {
    use time::{Duration, macros::datetime};

    use super::{DynamicLabel, insert_dynamic_label};

    fn label(offset: i64, transfers: u32, walking: u32) -> DynamicLabel {
        DynamicLabel {
            ready_at: datetime!(2026-08-31 10:00 UTC) + Duration::seconds(offset),
            transfer_count: transfers,
            walking_seconds: walking,
        }
    }

    #[test]
    fn dynamic_frontier_rejects_a_dominated_label_in_any_arrival_order() {
        let good = label(0, 0, 0);
        let bad = label(60, 1, 30);
        let mut good_first = vec![good];
        assert!(insert_dynamic_label(&mut good_first, bad).is_none());

        let mut bad_first = vec![bad];
        assert_eq!(insert_dynamic_label(&mut bad_first, good), Some(1));
        assert_eq!(bad_first.len(), 1);
        assert_eq!(bad_first[0].ready_at, good.ready_at);
        assert_eq!(bad_first[0].transfer_count, good.transfer_count);
        assert_eq!(bad_first[0].walking_seconds, good.walking_seconds);
    }

    #[test]
    fn dynamic_frontier_deduplicates_equal_labels() {
        let same = label(60, 1, 30);
        let mut labels = vec![same];
        assert!(insert_dynamic_label(&mut labels, same).is_none());
        assert_eq!(labels.len(), 1);
    }
}
