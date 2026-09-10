use std::{cmp::Ordering, collections::HashSet};

use serde::{Deserialize, Serialize};

use crate::{live_status::FreshnessState, normalization::ActionState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JourneyClass {
    Reliable,
    Feasible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JourneyLabel {
    BestNow,
    FewerTransfers,
    LessWalking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingReason {
    EarliestReliableArrival,
    FewerTransfers,
    LessWalking,
    TightTransferMargin,
    LowPickupBikes,
    LowReturnSlots,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BikeActionEvidence {
    pub pickup_state: ActionState,
    pub return_state: ActionState,
    pub available_bikes: Option<u32>,
    pub available_return_slots: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateJourney {
    pub stable_key: String,
    pub departure_epoch_seconds: i64,
    pub arrival_epoch_seconds: i64,
    pub walking_seconds: u32,
    pub transfer_count: u32,
    pub main_service_sequence: Vec<String>,
    pub required_live_status: Vec<FreshnessState>,
    pub transfers: Vec<TransferEvidence>,
    pub bike_availability: Option<BikeActionEvidence>,
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
    pub transfer_count: u32,
    pub main_service_sequence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankingPolicy {
    pub version: String,
    pub metro_transfer_buffer_seconds: u32,
    pub bus_transfer_buffer_seconds: u32,
    pub reliable_bike_count: u32,
    pub near_duplicate_arrival_seconds: i64,
    pub material_walking_difference_seconds: u32,
    pub excessive_alternative_delay_seconds: i64,
    pub result_limit: usize,
}

impl Default for RankingPolicy {
    fn default() -> Self {
        Self {
            version: "1".into(),
            metro_transfer_buffer_seconds: 180,
            bus_transfer_buffer_seconds: 300,
            reliable_bike_count: 3,
            near_duplicate_arrival_seconds: 300,
            material_walking_difference_seconds: 300,
            excessive_alternative_delay_seconds: 900,
            result_limit: 3,
        }
    }
}

impl RankingPolicy {
    pub fn rank(&self, candidates: Vec<CandidateJourney>) -> Vec<RankedJourney> {
        self.rank_with_schedule_only(candidates, &HashSet::new())
    }

    pub fn rank_with_schedule_only(
        &self,
        candidates: Vec<CandidateJourney>,
        schedule_only_keys: &HashSet<String>,
    ) -> Vec<RankedJourney> {
        let mut eligible: Vec<_> = candidates
            .into_iter()
            .filter_map(|candidate| self.classify(candidate, schedule_only_keys))
            .collect();

        eligible = remove_dominated(eligible);
        eligible.sort_by(compare_ranked);
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

    fn classify(
        &self,
        candidate: CandidateJourney,
        schedule_only_keys: &HashSet<String>,
    ) -> Option<RankedJourney> {
        if candidate.blocking_condition
            || candidate
                .required_live_status
                .iter()
                .any(|state| *state != FreshnessState::Fresh)
        {
            return None;
        }

        let mut class = if schedule_only_keys.contains(&candidate.stable_key) {
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

        if let Some(bike) = candidate.bike_availability {
            if bike.pickup_state != ActionState::Enabled
                || bike.return_state != ActionState::Enabled
                || bike.available_bikes == Some(0)
                || bike.available_return_slots == Some(0)
                || bike.available_bikes.is_none()
                || bike.available_return_slots.is_none()
            {
                return None;
            }
            if bike.available_bikes < Some(self.reliable_bike_count) {
                class = JourneyClass::Feasible;
                push_unique(&mut reasons, RankingReason::LowPickupBikes);
            }
            if bike.available_return_slots < Some(self.reliable_bike_count) {
                class = JourneyClass::Feasible;
                push_unique(&mut reasons, RankingReason::LowReturnSlots);
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
        let different_sequence = candidate.main_service_sequence != current.main_service_sequence;
        if !different_sequence && !fewer_transfers && !less_walking {
            return false;
        }
        candidate.arrival_epoch_seconds - current.arrival_epoch_seconds
            <= self.excessive_alternative_delay_seconds
            || fewer_transfers
            || less_walking
    }
}

fn compare_ranked(left: &RankedJourney, right: &RankedJourney) -> Ordering {
    left.class
        .cmp(&right.class)
        .then(left.arrival_epoch_seconds.cmp(&right.arrival_epoch_seconds))
        .then(left.transfer_count.cmp(&right.transfer_count))
        .then(left.walking_seconds.cmp(&right.walking_seconds))
        .then(
            left.departure_epoch_seconds
                .cmp(&right.departure_epoch_seconds),
        )
        .then(left.stable_key.cmp(&right.stable_key))
}

fn remove_dominated(candidates: Vec<RankedJourney>) -> Vec<RankedJourney> {
    candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            !candidates
                .iter()
                .enumerate()
                .any(|(other_index, other)| index != &other_index && dominates(other, candidate))
        })
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

fn dominates(left: &RankedJourney, right: &RankedJourney) -> bool {
    let no_worse = left.class <= right.class
        && left.arrival_epoch_seconds <= right.arrival_epoch_seconds
        && left.transfer_count <= right.transfer_count
        && left.walking_seconds <= right.walking_seconds;
    let strictly_better = left.class < right.class
        || left.arrival_epoch_seconds < right.arrival_epoch_seconds
        || left.transfer_count < right.transfer_count
        || left.walking_seconds < right.walking_seconds;
    no_worse && strictly_better
}

fn apply_labels(options: &mut [RankedJourney]) {
    let Some(best_index) = options
        .iter()
        .position(|option| option.class == JourneyClass::Reliable)
    else {
        return;
    };
    let best_transfers = options[best_index].transfer_count;
    let best_walking = options[best_index].walking_seconds;
    options[best_index].label = Some(JourneyLabel::BestNow);
    options[best_index]
        .ranking_reasons
        .insert(0, RankingReason::EarliestReliableArrival);
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
