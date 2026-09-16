use std::collections::HashSet;

use via_server::domain::realtime::ActionState;
use via_server::domain::{
    ranking::{
        BikeActionEvidence, CandidateJourney, JourneyClass, JourneyLabel, RankingPolicy,
        RankingReason, TransferEvidence, TransferKind,
    },
    realtime::FreshnessState,
};

fn candidate(key: &str, arrival: i64) -> CandidateJourney {
    CandidateJourney {
        stable_key: key.into(),
        departure_epoch_seconds: 1_000,
        arrival_epoch_seconds: arrival,
        walking_seconds: 300,
        bike_share_seconds: 0,
        transfer_count: 1,
        main_service_sequence: vec![key.into()],
        required_live_status: vec![FreshnessState::Fresh],
        transfers: Vec::new(),
        bike_availability: None,
        blocking_condition: false,
    }
}

#[test]
fn required_live_status_is_an_eligibility_gate() {
    let mut stale = candidate("stale", 2_000);
    stale.required_live_status = vec![FreshnessState::Degraded];
    let fresh = candidate("fresh", 2_100);

    let ranked = RankingPolicy::default().rank(vec![stale, fresh]);

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].stable_key, "fresh");
}

#[test]
fn transfer_safety_boundaries_classify_exactly() {
    let mut metro_reliable = candidate("metro-reliable", 2_000);
    metro_reliable.transfers = vec![TransferEvidence {
        kind: TransferKind::MetroToMetro,
        physical_seconds: 120,
        available_seconds: 300,
    }];
    let mut bus_feasible = candidate("bus-feasible", 1_900);
    bus_feasible.transfers = vec![TransferEvidence {
        kind: TransferKind::BusInvolved,
        physical_seconds: 120,
        available_seconds: 419,
    }];
    let mut impossible = candidate("impossible", 1_800);
    impossible.transfers = vec![TransferEvidence {
        kind: TransferKind::MetroToMetro,
        physical_seconds: 120,
        available_seconds: 119,
    }];

    let ranked = RankingPolicy::default().rank(vec![bus_feasible, impossible, metro_reliable]);

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].stable_key, "metro-reliable");
    assert_eq!(ranked[0].class, JourneyClass::Reliable);
    assert_eq!(ranked[1].stable_key, "bus-feasible");
    assert_eq!(ranked[1].class, JourneyClass::Feasible);
}

#[test]
fn youbike_zero_one_two_and_three_thresholds_are_exact() {
    let options = [0, 1, 2, 3]
        .into_iter()
        .map(|count| {
            let arrival = if count == 3 {
                2_900
            } else {
                2_000 + i64::from(count)
            };
            let mut option = candidate(&format!("bike-{count}"), arrival);
            option.walking_seconds = match count {
                1 => 500,
                2 => 300,
                3 => 200,
                _ => 600,
            };
            option.main_service_sequence = vec![format!("bike-{count}")];
            option.bike_availability = Some(BikeActionEvidence {
                pickup_state: ActionState::Enabled,
                return_state: ActionState::Enabled,
                available_bikes: Some(count),
                available_return_slots: Some(count),
            });
            option
        })
        .collect();

    let ranked = RankingPolicy::default().rank(options);

    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].stable_key, "bike-1");
    assert_eq!(ranked[0].class, JourneyClass::Feasible);
    assert_eq!(ranked[1].class, JourneyClass::Feasible);
    assert_eq!(ranked[2].class, JourneyClass::Reliable);
}

#[test]
fn dominance_near_duplicates_and_labels_are_server_owned() {
    let best = candidate("blue-line", 2_000);
    let mut duplicate = candidate("blue-line", 2_240);
    duplicate.walking_seconds = 400;
    let mut fewer_transfers = candidate("307", 2_500);
    fewer_transfers.transfer_count = 0;
    let mut less_walking = candidate("green-line", 2_600);
    less_walking.walking_seconds = 0;

    let ranked =
        RankingPolicy::default().rank(vec![less_walking, duplicate, best, fewer_transfers]);

    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].stable_key, "blue-line");
    assert_eq!(ranked[0].label, Some(JourneyLabel::BestNow));
    assert_eq!(ranked[1].label, Some(JourneyLabel::FewerTransfers));
    assert_eq!(ranked[2].label, Some(JourneyLabel::LessWalking));
}

#[test]
fn ranking_replay_is_deterministic() {
    let input = vec![candidate("b", 2_000), candidate("a", 2_000)];
    let first = serde_json::to_vec(&RankingPolicy::default().rank(input.clone())).unwrap();
    let second = serde_json::to_vec(&RankingPolicy::default().rank(input)).unwrap();
    assert_eq!(first, second);
}

#[test]
fn materially_earlier_feasible_journey_beats_reliable_journey() {
    let mut walking = candidate("direct-walking", 65 * 60);
    walking.walking_seconds = 65 * 60;
    walking.transfer_count = 0;
    walking.main_service_sequence.clear();

    let mut metro = candidate("schedule-only-metro", 12 * 60);
    metro.walking_seconds = 240;
    metro.transfer_count = 0;
    metro.main_service_sequence = vec!["metro-blue".into()];
    let schedule_only = HashSet::from([metro.stable_key.clone()]);

    let ranked =
        RankingPolicy::default().rank_with_schedule_only(vec![walking, metro], &schedule_only);

    assert_eq!(ranked[0].stable_key, "schedule-only-metro");
    assert_eq!(ranked[0].class, JourneyClass::Feasible);
    assert_eq!(ranked[0].label, Some(JourneyLabel::BestNow));
    assert_eq!(ranked[0].ranking_reasons[0], RankingReason::EarliestArrival);
}

#[test]
fn reliable_journey_wins_when_arrivals_are_within_reliability_window() {
    let reliable = candidate("reliable", 13 * 60);
    let feasible = candidate("schedule-only", 12 * 60);
    let schedule_only = HashSet::from([feasible.stable_key.clone()]);

    let ranked =
        RankingPolicy::default().rank_with_schedule_only(vec![feasible, reliable], &schedule_only);

    assert_eq!(ranked[0].stable_key, "reliable");
    assert_eq!(ranked[0].class, JourneyClass::Reliable);
    assert_eq!(ranked[1].class, JourneyClass::Feasible);
}

#[test]
fn reliability_preference_window_is_configurable() {
    let mut reliable = candidate("reliable", 13 * 60);
    reliable.transfer_count = 0;
    reliable.walking_seconds = 0;
    let mut feasible = candidate("schedule-only", 12 * 60);
    feasible.transfer_count = 1;
    feasible.walking_seconds = 600;
    let schedule_only = HashSet::from([feasible.stable_key.clone()]);
    let policy = RankingPolicy {
        reliability_preference_window_seconds: 30,
        ..RankingPolicy::default()
    };

    let ranked = policy.rank_with_schedule_only(vec![reliable, feasible], &schedule_only);

    assert_eq!(ranked[0].stable_key, "schedule-only");
}

#[test]
fn feasible_journey_wins_when_arrival_difference_is_material() {
    let reliable = candidate("reliable", 30 * 60);
    let mut feasible = candidate("schedule-only", 15 * 60);
    feasible.transfer_count = 1;
    feasible.walking_seconds = 600;
    let schedule_only = HashSet::from([feasible.stable_key.clone()]);

    let ranked =
        RankingPolicy::default().rank_with_schedule_only(vec![reliable, feasible], &schedule_only);

    assert_eq!(ranked[0].stable_key, "schedule-only");
    assert_eq!(ranked[0].class, JourneyClass::Feasible);
}

#[test]
fn same_class_orders_earlier_arrival_first() {
    let later = candidate("later", 2_000);
    let earlier = candidate("earlier", 1_900);

    let ranked = RankingPolicy::default().rank(vec![later, earlier]);

    assert_eq!(ranked[0].stable_key, "earlier");
}

#[test]
fn same_arrival_and_class_prefers_fewer_transfers_then_less_walking() {
    let mut fewer_transfers = candidate("fewer-transfers", 2_000);
    fewer_transfers.transfer_count = 1;
    fewer_transfers.walking_seconds = 600;

    let mut less_walking = candidate("less-walking", 2_000);
    less_walking.transfer_count = 2;
    less_walking.walking_seconds = 0;

    let ranked = RankingPolicy::default().rank(vec![less_walking, fewer_transfers]);

    assert_eq!(ranked[0].stable_key, "fewer-transfers");
    assert_eq!(ranked[1].stable_key, "less-walking");
}

#[test]
fn near_duplicates_keep_the_option_preferred_by_the_ranking_policy() {
    let mut reliable = candidate("reliable", 13 * 60);
    reliable.main_service_sequence = vec!["shared-service".into()];
    let mut feasible = candidate("schedule-only", 12 * 60);
    feasible.main_service_sequence = vec!["shared-service".into()];
    let schedule_only = HashSet::from([feasible.stable_key.clone()]);

    let ranked =
        RankingPolicy::default().rank_with_schedule_only(vec![feasible, reliable], &schedule_only);

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].stable_key, "reliable");
}
