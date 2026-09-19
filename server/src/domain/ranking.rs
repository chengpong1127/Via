use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::domain::{
    realtime::FreshnessState,
    transit_timing::{TimingBasis, TimingReliability},
};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum JourneyClass {
    Reliable,
    Feasible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JourneyLabel {
    BestNow,
    FewerTransfers,
    LessWalking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RankingReason {
    EarliestArrival,
    EarliestReliableArrival,
    FewerTransfers,
    LessWalking,
    TightTransferMargin,
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
pub struct CandidateJourney {
    pub stable_key: String,
    pub departure_epoch_seconds: i64,
    pub arrival_epoch_seconds: i64,
    pub walking_seconds: u32,
    pub bicycle_seconds: u32,
    pub bike_share_uses: u32,
    pub transfer_count: u32,
    pub main_service_sequence: Vec<String>,
    pub timing_bases: Vec<TimingBasis>,
    pub timing_reliability: TimingReliability,
    pub required_live_status: Vec<FreshnessState>,
    pub transfers: Vec<TransferEvidence>,
    pub blocking_condition: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankedJourney {
    pub stable_key: String,
    pub class: JourneyClass,
    pub label: Option<JourneyLabel>,
    pub ranking_reasons: Vec<RankingReason>,
    pub departure_epoch_seconds: i64,
    pub arrival_epoch_seconds: i64,
    pub walking_seconds: u32,
    pub bicycle_seconds: u32,
    pub bike_share_uses: u32,
    pub transfer_count: u32,
    pub main_service_sequence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankingPolicy {
    pub version: String,
    pub metro_transfer_buffer_seconds: u32,
    pub bus_transfer_buffer_seconds: u32,
    pub near_duplicate_arrival_seconds: i64,
    pub material_walking_difference_seconds: u32,
    pub excessive_alternative_delay_seconds: i64,
    /// Reliability can outweigh an arrival advantage inside this window.
    pub reliability_preference_window_seconds: u32,
    pub result_limit: usize,
}

impl Default for RankingPolicy {
    fn default() -> Self {
        Self {
            version: "1".into(),
            metro_transfer_buffer_seconds: 180,
            bus_transfer_buffer_seconds: 300,
            near_duplicate_arrival_seconds: 300,
            material_walking_difference_seconds: 300,
            excessive_alternative_delay_seconds: 900,
            reliability_preference_window_seconds: 300,
            result_limit: 3,
        }
    }
}

impl RankingPolicy {
    pub fn rank(&self, candidates: Vec<CandidateJourney>) -> Vec<RankedJourney> {
        let mut eligible: Vec<_> = candidates
            .into_iter()
            .filter_map(|candidate| self.classify(candidate))
            .collect();

        eligible = remove_dominated(self, eligible);
        eligible.sort_by(|left, right| self.compare_ranked(left, right));
        eligible = self.remove_near_duplicates(eligible);

        let mut selected = Vec::with_capacity(self.result_limit);
        for candidate in eligible {
            if selected.len() == self.result_limit {
                break;
            }
            if selected
                .iter()
                .all(|current| self.materially_different(current, &candidate))
            {
                selected.push(candidate);
            }
        }
        apply_labels(&mut selected);
        selected
    }

    fn classify(&self, candidate: CandidateJourney) -> Option<RankedJourney> {
        if candidate.blocking_condition
            || candidate
                .required_live_status
                .iter()
                .any(|state| *state != FreshnessState::Fresh)
        {
            return None;
        }

        let mut class = if candidate.timing_reliability == TimingReliability::Fallback {
            JourneyClass::Feasible
        } else {
            JourneyClass::Reliable
        };
        let mut reasons = Vec::new();
        for transfer in &candidate.transfers {
            if transfer.available_seconds < transfer.physical_seconds {
                return None;
            }
            let buffer = match transfer.kind {
                TransferKind::MetroToMetro => self.metro_transfer_buffer_seconds,
                TransferKind::BusInvolved => self.bus_transfer_buffer_seconds,
            };
            if transfer.available_seconds < transfer.physical_seconds.saturating_add(buffer) {
                class = JourneyClass::Feasible;
                push_unique(&mut reasons, RankingReason::TightTransferMargin);
            }
        }

        Some(RankedJourney {
            stable_key: candidate.stable_key,
            class,
            label: None,
            ranking_reasons: reasons,
            departure_epoch_seconds: candidate.departure_epoch_seconds,
            arrival_epoch_seconds: candidate.arrival_epoch_seconds,
            walking_seconds: candidate.walking_seconds,
            bicycle_seconds: candidate.bicycle_seconds,
            bike_share_uses: candidate.bike_share_uses,
            transfer_count: candidate.transfer_count,
            main_service_sequence: candidate.main_service_sequence,
        })
    }

    fn remove_near_duplicates(&self, candidates: Vec<RankedJourney>) -> Vec<RankedJourney> {
        let mut kept = Vec::new();
        for candidate in candidates {
            let duplicate = kept.iter().any(|current: &RankedJourney| {
                current.main_service_sequence == candidate.main_service_sequence
                    && current.transfer_count == candidate.transfer_count
                    && (current.arrival_epoch_seconds - candidate.arrival_epoch_seconds).abs()
                        <= self.near_duplicate_arrival_seconds
                    && current.walking_seconds.abs_diff(candidate.walking_seconds)
                        < self.material_walking_difference_seconds
                    && current.bicycle_seconds == candidate.bicycle_seconds
                    && current.bike_share_uses == candidate.bike_share_uses
            });
            if !duplicate {
                kept.push(candidate);
            }
        }
        kept
    }

    fn materially_different(&self, current: &RankedJourney, candidate: &RankedJourney) -> bool {
        let fewer_transfers = candidate.transfer_count < current.transfer_count;
        let less_walking = current
            .walking_seconds
            .saturating_sub(candidate.walking_seconds)
            >= self.material_walking_difference_seconds;
        let different_bicycle = candidate.bicycle_seconds != current.bicycle_seconds
            || candidate.bike_share_uses != current.bike_share_uses;
        let different_sequence = candidate.main_service_sequence != current.main_service_sequence;
        if !different_sequence && !fewer_transfers && !less_walking && !different_bicycle {
            return false;
        }
        candidate.arrival_epoch_seconds - current.arrival_epoch_seconds
            <= self.excessive_alternative_delay_seconds
            || fewer_transfers
            || less_walking
            || different_bicycle
    }

    fn compare_ranked(&self, left: &RankedJourney, right: &RankedJourney) -> Ordering {
        compare_primary_preference(self, left, right)
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

fn compare_primary_preference(
    policy: &RankingPolicy,
    left: &RankedJourney,
    right: &RankedJourney,
) -> Ordering {
    let arrival_difference = left
        .arrival_epoch_seconds
        .abs_diff(right.arrival_epoch_seconds);
    if left.class != right.class
        && arrival_difference <= u64::from(policy.reliability_preference_window_seconds)
    {
        compare_classes(left.class, right.class)
    } else {
        left.arrival_epoch_seconds.cmp(&right.arrival_epoch_seconds)
    }
}

fn compare_classes(left: JourneyClass, right: JourneyClass) -> Ordering {
    match (left, right) {
        (JourneyClass::Reliable, JourneyClass::Feasible) => Ordering::Less,
        (JourneyClass::Feasible, JourneyClass::Reliable) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

fn remove_dominated(policy: &RankingPolicy, candidates: Vec<RankedJourney>) -> Vec<RankedJourney> {
    candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            !candidates.iter().enumerate().any(|(other_index, other)| {
                index != &other_index && dominates(policy, other, candidate)
            })
        })
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

fn dominates(policy: &RankingPolicy, left: &RankedJourney, right: &RankedJourney) -> bool {
    let no_worse = class_is_no_worse(policy, left, right)
        && left.arrival_epoch_seconds <= right.arrival_epoch_seconds
        && left.transfer_count <= right.transfer_count
        && left.walking_seconds <= right.walking_seconds
        && left.bicycle_seconds <= right.bicycle_seconds
        && left.bike_share_uses <= right.bike_share_uses;
    let strictly_better = class_is_strictly_better(policy, left, right)
        || left.arrival_epoch_seconds < right.arrival_epoch_seconds
        || left.transfer_count < right.transfer_count
        || left.walking_seconds < right.walking_seconds
        || left.bicycle_seconds < right.bicycle_seconds
        || left.bike_share_uses < right.bike_share_uses;
    no_worse && strictly_better
}

fn class_is_no_worse(policy: &RankingPolicy, left: &RankedJourney, right: &RankedJourney) -> bool {
    compare_primary_preference(policy, left, right) != Ordering::Greater
}

fn class_is_strictly_better(
    policy: &RankingPolicy,
    left: &RankedJourney,
    right: &RankedJourney,
) -> bool {
    left.class != right.class && compare_primary_preference(policy, left, right) == Ordering::Less
}

fn apply_labels(options: &mut [RankedJourney]) {
    if options.is_empty() {
        return;
    }
    let best_index = 0;
    let best_transfers = options[best_index].transfer_count;
    let best_walking = options[best_index].walking_seconds;
    let best_reason = match options[best_index].class {
        JourneyClass::Reliable => RankingReason::EarliestReliableArrival,
        JourneyClass::Feasible => RankingReason::EarliestArrival,
    };
    options[best_index].label = Some(JourneyLabel::BestNow);
    options[best_index].ranking_reasons.insert(0, best_reason);
    for (index, option) in options.iter_mut().enumerate() {
        if index == best_index {
            continue;
        }
        if option.transfer_count < best_transfers {
            option.label = Some(JourneyLabel::FewerTransfers);
            option
                .ranking_reasons
                .insert(0, RankingReason::FewerTransfers);
        } else if option.walking_seconds < best_walking {
            option.label = Some(JourneyLabel::LessWalking);
            option.ranking_reasons.insert(0, RankingReason::LessWalking);
        }
    }
}

fn push_unique(reasons: &mut Vec<RankingReason>, reason: RankingReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}
