use std::cmp::Ordering;

use crate::domain::transit::timing::TimingReliability;

pub const MAX_RESULT_LIMIT: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum JourneyClass {
    Reliable,
    Feasible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferKind {
    MetroToMetro,
    BusInvolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferEvidence {
    pub kind: TransferKind,
    pub physical_seconds: u32,
    pub available_seconds: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankingCandidate {
    pub stable_key: String,
    pub departure_epoch_seconds: i64,
    pub arrival_epoch_seconds: i64,
    pub walking_seconds: u32,
    pub bicycle_seconds: u32,
    pub bike_share_uses: u32,
    pub transfer_count: u32,
    pub main_pattern_sequence: Vec<String>,
    pub timing_reliability: TimingReliability,
    pub transfers: Vec<TransferEvidence>,
}

impl AsRef<RankingCandidate> for RankingCandidate {
    fn as_ref(&self) -> &RankingCandidate {
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedJourney<T = RankingCandidate> {
    pub class: JourneyClass,
    pub candidate: T,
}

impl<T: AsRef<RankingCandidate>> RankedJourney<T> {
    fn metrics(&self) -> &RankingCandidate {
        self.candidate.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RankingPolicy {
    pub metro_transfer_buffer_seconds: u32,
    pub bus_transfer_buffer_seconds: u32,
    pub material_walking_difference_seconds: u32,
    pub max_sequence_only_option_delay_seconds: i64,

    pub reliability_preference_window_seconds: u32,
    pub result_limit: usize,
}

impl Default for RankingPolicy {
    fn default() -> Self {
        Self {
            metro_transfer_buffer_seconds: 180,
            bus_transfer_buffer_seconds: 300,
            material_walking_difference_seconds: 300,
            max_sequence_only_option_delay_seconds: 900,
            reliability_preference_window_seconds: 300,
            result_limit: MAX_RESULT_LIMIT,
        }
    }
}

impl RankingPolicy {
    pub fn rank<T: AsRef<RankingCandidate>>(&self, candidates: Vec<T>) -> Vec<RankedJourney<T>> {
        let mut eligible_candidates: Vec<_> = candidates
            .into_iter()
            .filter_map(|candidate| self.classify(candidate))
            .collect();

        eligible_candidates = remove_dominated(self, eligible_candidates);
        eligible_candidates.sort_by(|left, right| self.compare_ranked(left, right));

        let mut selected_journeys = Vec::with_capacity(self.result_limit);
        for candidate in eligible_candidates {
            if selected_journeys.len() == self.result_limit {
                break;
            }
            if selected_journeys
                .iter()
                .all(|current| self.materially_different(current, &candidate))
            {
                selected_journeys.push(candidate);
            }
        }
        selected_journeys
    }

    fn classify<T: AsRef<RankingCandidate>>(&self, candidate: T) -> Option<RankedJourney<T>> {
        let metrics = candidate.as_ref();
        let mut class = if metrics.timing_reliability == TimingReliability::Fallback {
            JourneyClass::Feasible
        } else {
            JourneyClass::Reliable
        };
        for transfer in &metrics.transfers {
            if transfer.available_seconds < transfer.physical_seconds {
                return None;
            }
            let buffer = match transfer.kind {
                TransferKind::MetroToMetro => self.metro_transfer_buffer_seconds,
                TransferKind::BusInvolved => self.bus_transfer_buffer_seconds,
            };
            if transfer.available_seconds < transfer.physical_seconds.saturating_add(buffer) {
                class = JourneyClass::Feasible;
            }
        }

        Some(RankedJourney { class, candidate })
    }

    fn materially_different<T: AsRef<RankingCandidate>>(
        &self,
        current: &RankedJourney<T>,
        candidate: &RankedJourney<T>,
    ) -> bool {
        let (current, candidate) = (current.metrics(), candidate.metrics());
        let fewer_transfers = candidate.transfer_count < current.transfer_count;
        let less_walking = current
            .walking_seconds
            .saturating_sub(candidate.walking_seconds)
            >= self.material_walking_difference_seconds;
        let different_bicycle = !self.same_bicycle_use(current, candidate);
        let different_sequence = candidate.main_pattern_sequence != current.main_pattern_sequence;
        if !different_sequence && !fewer_transfers && !less_walking && !different_bicycle {
            return false;
        }
        let more_walking = candidate
            .walking_seconds
            .saturating_sub(current.walking_seconds)
            >= self.material_walking_difference_seconds;
        let more_transfers = candidate.transfer_count > current.transfer_count;
        candidate.arrival_epoch_seconds - current.arrival_epoch_seconds
            <= self.max_sequence_only_option_delay_seconds
            || (fewer_transfers && !more_walking)
            || (less_walking && !more_transfers)
    }

    fn same_bicycle_use(&self, current: &RankingCandidate, candidate: &RankingCandidate) -> bool {
        current.bike_share_uses == candidate.bike_share_uses
            && current.bicycle_seconds.abs_diff(candidate.bicycle_seconds)
                < self.material_walking_difference_seconds
    }

    fn compare_ranked<T: AsRef<RankingCandidate>>(
        &self,
        left: &RankedJourney<T>,
        right: &RankedJourney<T>,
    ) -> Ordering {
        let primary = compare_primary_preference(self, left, right);
        let (left, right) = (left.metrics(), right.metrics());
        primary
            .then(left.transfer_count.cmp(&right.transfer_count))
            .then(left.walking_seconds.cmp(&right.walking_seconds))
            .then(left.bicycle_seconds.cmp(&right.bicycle_seconds))
            .then(left.bike_share_uses.cmp(&right.bike_share_uses))
            .then(left.arrival_epoch_seconds.cmp(&right.arrival_epoch_seconds))
            .then(
                left.departure_epoch_seconds
                    .cmp(&right.departure_epoch_seconds),
            )
            .then(left.stable_key.cmp(&right.stable_key))
    }
}

fn compare_primary_preference<T: AsRef<RankingCandidate>>(
    policy: &RankingPolicy,
    left: &RankedJourney<T>,
    right: &RankedJourney<T>,
) -> Ordering {
    let (left_arrival, right_arrival) = (
        left.metrics().arrival_epoch_seconds,
        right.metrics().arrival_epoch_seconds,
    );
    if left.class != right.class
        && left_arrival.abs_diff(right_arrival)
            <= u64::from(policy.reliability_preference_window_seconds)
    {
        compare_classes(left.class, right.class)
    } else {
        left_arrival.cmp(&right_arrival)
    }
}

fn compare_classes(left: JourneyClass, right: JourneyClass) -> Ordering {
    match (left, right) {
        (JourneyClass::Reliable, JourneyClass::Feasible) => Ordering::Less,
        (JourneyClass::Feasible, JourneyClass::Reliable) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

fn remove_dominated<T: AsRef<RankingCandidate>>(
    policy: &RankingPolicy,
    candidates: Vec<RankedJourney<T>>,
) -> Vec<RankedJourney<T>> {
    let keep = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            !candidates.iter().enumerate().any(|(other_index, other)| {
                index != other_index && dominates(policy, other, candidate)
            })
        })
        .collect::<Vec<_>>();
    candidates
        .into_iter()
        .zip(keep)
        .filter_map(|(candidate, keep)| keep.then_some(candidate))
        .collect()
}

fn dominates<T: AsRef<RankingCandidate>>(
    policy: &RankingPolicy,
    left: &RankedJourney<T>,
    right: &RankedJourney<T>,
) -> bool {
    let class_no_worse = class_is_no_worse(policy, left, right);
    let class_strictly_better = class_is_strictly_better(policy, left, right);
    let (left, right) = (left.metrics(), right.metrics());
    let no_worse = class_no_worse
        && left.arrival_epoch_seconds <= right.arrival_epoch_seconds
        && left.transfer_count <= right.transfer_count
        && left.walking_seconds <= right.walking_seconds
        && left.bicycle_seconds <= right.bicycle_seconds
        && left.bike_share_uses <= right.bike_share_uses;
    let strictly_better = class_strictly_better
        || left.arrival_epoch_seconds < right.arrival_epoch_seconds
        || left.transfer_count < right.transfer_count
        || left.walking_seconds < right.walking_seconds
        || left.bicycle_seconds < right.bicycle_seconds
        || left.bike_share_uses < right.bike_share_uses;
    no_worse && strictly_better
}

fn class_is_no_worse<T: AsRef<RankingCandidate>>(
    policy: &RankingPolicy,
    left: &RankedJourney<T>,
    right: &RankedJourney<T>,
) -> bool {
    compare_primary_preference(policy, left, right) != Ordering::Greater
}

fn class_is_strictly_better<T: AsRef<RankingCandidate>>(
    policy: &RankingPolicy,
    left: &RankedJourney<T>,
    right: &RankedJourney<T>,
) -> bool {
    left.class != right.class && compare_primary_preference(policy, left, right) == Ordering::Less
}
