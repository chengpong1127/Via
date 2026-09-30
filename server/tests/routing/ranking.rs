use via_server::domain::{
    ranking::{
        CandidateJourney, JourneyClass, JourneyLabel, RankingPolicy, RankingReason,
        TransferEvidence, TransferKind,
    },
    realtime::FreshnessState,
    transit_timing::{TimingBasis, TimingReliability},
};

fn candidate(key: &str, arrival: i64) -> CandidateJourney {
    CandidateJourney {
        stable_key: key.into(),
        departure_epoch_seconds: 1_000,
        arrival_epoch_seconds: arrival,
        walking_seconds: 300,
        bicycle_seconds: 0,
        bike_share_uses: 0,
        transfer_count: 1,
        main_pattern_sequence: vec![key.into()],
        timing_bases: Vec::new(),
        timing_reliability: TimingReliability::Reliable,
        required_live_status: vec![FreshnessState::Fresh],
        transfers: Vec::new(),
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
fn timing_reliability_is_separate_from_timing_basis() {
    let mut realtime = candidate("realtime", 2_000);
    realtime.timing_bases = vec![TimingBasis::RealtimePrediction];
    realtime.walking_seconds = 500;
    let mut delay_adjusted = candidate("delay-adjusted", 2_001);
    delay_adjusted.timing_bases = vec![TimingBasis::DelayAdjustedSchedule];
    delay_adjusted.walking_seconds = 400;
    let mut schedule = candidate("schedule", 2_002);
    schedule.timing_bases = vec![TimingBasis::Schedule];
    schedule.walking_seconds = 300;
    let mut frequency = candidate("frequency", 2_003);
    frequency.timing_bases = vec![TimingBasis::FrequencyEstimate];
    frequency.timing_reliability = TimingReliability::Fallback;
    frequency.walking_seconds = 200;
    let mut mixed = candidate("mixed", 2_004);
    mixed.timing_bases = vec![TimingBasis::RealtimePrediction, TimingBasis::Schedule];
    mixed.timing_reliability = TimingReliability::Fallback;
    mixed.walking_seconds = 100;

    let ranked = RankingPolicy {
        result_limit: 5,
        ..RankingPolicy::default()
    }
    .rank(vec![realtime, delay_adjusted, schedule, frequency, mixed]);

    let classes = ranked
        .iter()
        .map(|journey| (journey.stable_key.as_str(), journey.class))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(classes["realtime"], JourneyClass::Reliable);
    assert_eq!(classes["delay-adjusted"], JourneyClass::Reliable);
    assert_eq!(classes["schedule"], JourneyClass::Reliable);
    assert_eq!(classes["frequency"], JourneyClass::Feasible);
    assert_eq!(classes["mixed"], JourneyClass::Feasible);
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
fn direct_through_journey_dominates_same_arrival_reboarding_route() {
    let mut direct = candidate("red-through", 2_000);
    direct.transfer_count = 0;
    direct.main_pattern_sequence = vec!["red-full".into()];
    direct.timing_bases = vec![TimingBasis::Schedule];
    direct.timing_reliability = TimingReliability::Fallback;

    let mut reboarding = candidate("red-reboard", 2_000);
    reboarding.transfer_count = 1;
    reboarding.main_pattern_sequence = vec!["red-short", "red-full"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    reboarding.timing_bases = vec![TimingBasis::Schedule, TimingBasis::Schedule];
    reboarding.timing_reliability = TimingReliability::Fallback;

    let ranked = RankingPolicy::default().rank(vec![reboarding, direct]);

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].stable_key, "red-through");
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
    walking.main_pattern_sequence.clear();

    let mut metro = candidate("schedule-only-metro", 12 * 60);
    metro.walking_seconds = 240;
    metro.transfer_count = 0;
    metro.main_pattern_sequence = vec!["metro-blue".into()];
    metro.timing_bases = vec![TimingBasis::Schedule];
    metro.timing_reliability = TimingReliability::Fallback;

    let ranked = RankingPolicy::default().rank(vec![walking, metro]);

    assert_eq!(ranked[0].stable_key, "schedule-only-metro");
    assert_eq!(ranked[0].class, JourneyClass::Feasible);
    assert_eq!(ranked[0].label, Some(JourneyLabel::BestNow));
    assert_eq!(ranked[0].ranking_reasons[0], RankingReason::EarliestArrival);
}

#[test]
fn reliable_journey_wins_when_arrivals_are_within_reliability_window() {
    let reliable = candidate("reliable", 13 * 60);
    let mut feasible = candidate("schedule-only", 12 * 60);
    feasible.timing_bases = vec![TimingBasis::Schedule];
    feasible.timing_reliability = TimingReliability::Fallback;

    let ranked = RankingPolicy::default().rank(vec![feasible, reliable]);

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
    feasible.timing_bases = vec![TimingBasis::Schedule];
    feasible.timing_reliability = TimingReliability::Fallback;
    let policy = RankingPolicy {
        reliability_preference_window_seconds: 30,
        ..RankingPolicy::default()
    };

    let ranked = policy.rank(vec![reliable, feasible]);

    assert_eq!(ranked[0].stable_key, "schedule-only");
}

#[test]
fn feasible_journey_wins_when_arrival_difference_is_material() {
    let reliable = candidate("reliable", 30 * 60);
    let mut feasible = candidate("schedule-only", 15 * 60);
    feasible.transfer_count = 1;
    feasible.walking_seconds = 600;
    feasible.timing_bases = vec![TimingBasis::Schedule];
    feasible.timing_reliability = TimingReliability::Fallback;

    let ranked = RankingPolicy::default().rank(vec![reliable, feasible]);

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
fn bike_aware_ranking_keeps_faster_bike_and_slower_zero_bike_options() {
    let mut bike = candidate("bike", 1_800);
    bike.walking_seconds = 180;
    bike.bicycle_seconds = 420;
    bike.bike_share_uses = 1;
    bike.main_pattern_sequence = vec!["shared-service".into()];

    let mut transit = candidate("transit", 2_000);
    transit.walking_seconds = 300;
    transit.bicycle_seconds = 0;
    transit.bike_share_uses = 0;
    transit.main_pattern_sequence = vec!["shared-service".into()];

    let ranked = RankingPolicy::default().rank(vec![transit, bike]);

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].stable_key, "bike");
    assert_eq!(ranked[1].stable_key, "transit");
}

#[test]
fn near_duplicates_keep_the_option_preferred_by_the_ranking_policy() {
    let mut reliable = candidate("reliable", 13 * 60);
    reliable.main_pattern_sequence = vec!["shared-service".into()];
    let mut feasible = candidate("schedule-only", 12 * 60);
    feasible.main_pattern_sequence = vec!["shared-service".into()];
    feasible.timing_bases = vec![TimingBasis::Schedule];
    feasible.timing_reliability = TimingReliability::Fallback;

    let ranked = RankingPolicy::default().rank(vec![feasible, reliable]);

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].stable_key, "reliable");
}
