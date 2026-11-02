use via_server::domain::{
    ranking::{
        JourneyClass, JourneyLabel, RankingCandidate, RankingPolicy, TransferEvidence, TransferKind,
    },
    realtime::FreshnessState,
    transit_timing::{TimingBasis, TimingReliability},
};

fn candidate(key: &str, arrival: i64) -> RankingCandidate {
    RankingCandidate {
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
    let expected = RankingPolicy::default().rank(input.clone());
    assert_eq!(
        expected
            .iter()
            .map(|value| value.stable_key.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    for candidates in [input.clone(), input.into_iter().rev().collect()] {
        assert_eq!(RankingPolicy::default().rank(candidates), expected);
    }
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
    assert_eq!(ranked[0].label, Some(JourneyLabel::BestNow));
}

#[test]
fn reliability_preference_obeys_exact_window_boundaries() {
    let default_window = RankingPolicy::default().reliability_preference_window_seconds;
    for (gap, window, expected_key) in [
        (60, default_window, "reliable"),
        (300, 300, "reliable"),
        (301, 300, "schedule-only"),
        (30, 30, "reliable"),
        (31, 30, "schedule-only"),
    ] {
        let reliable = candidate("reliable", 12 * 60 + gap);
        let mut feasible = candidate("schedule-only", 12 * 60);
        feasible.timing_bases = vec![TimingBasis::Schedule];
        feasible.timing_reliability = TimingReliability::Fallback;
        let policy = RankingPolicy {
            reliability_preference_window_seconds: window,
            ..RankingPolicy::default()
        };

        let ranked = policy.rank(vec![feasible, reliable]);

        assert_eq!(
            ranked[0].stable_key, expected_key,
            "gap={gap}, window={window}"
        );
        assert_eq!(
            ranked
                .iter()
                .find(|value| value.stable_key == "schedule-only")
                .unwrap()
                .class,
            JourneyClass::Feasible,
            "gap={gap}, window={window}"
        );
    }
}

#[test]
fn earlier_feasible_journey_wins_outside_the_configured_reliability_window() {
    let default_window = RankingPolicy::default().reliability_preference_window_seconds;
    for (reliable_arrival, feasible_arrival, window, transfers, walking) in [
        (13 * 60, 12 * 60, 30, 0, 0),
        (30 * 60, 15 * 60, default_window, 1, 300),
    ] {
        let mut reliable = candidate("reliable", reliable_arrival);
        reliable.transfer_count = transfers;
        reliable.walking_seconds = walking;
        let mut feasible = candidate("schedule-only", feasible_arrival);
        feasible.walking_seconds = 600;
        feasible.timing_bases = vec![TimingBasis::Schedule];
        feasible.timing_reliability = TimingReliability::Fallback;
        let policy = RankingPolicy {
            reliability_preference_window_seconds: window,
            ..RankingPolicy::default()
        };

        let ranked = policy.rank(vec![reliable, feasible]);

        assert_eq!(ranked[0].stable_key, "schedule-only", "window={window}");
        assert_eq!(ranked[0].class, JourneyClass::Feasible, "window={window}");
    }
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

    let mut more_walking = candidate("more-walking", 2_000);
    more_walking.transfer_count = 2;
    more_walking.walking_seconds = 600;
    let mut less_walking = candidate("less-walking", 2_000);
    less_walking.transfer_count = 2;
    less_walking.walking_seconds = 0;
    less_walking.bicycle_seconds = 600;
    less_walking.bike_share_uses = 1;

    let ranked = RankingPolicy::default().rank(vec![more_walking, less_walking]);

    assert_eq!(
        ranked
            .iter()
            .map(|value| value.stable_key.as_str())
            .collect::<Vec<_>>(),
        vec!["less-walking", "more-walking"]
    );
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

fn bike_only(key: &str, arrival: i64, walking: u32, bicycle: u32) -> RankingCandidate {
    let mut journey = candidate(key, arrival);
    journey.walking_seconds = walking;
    journey.bicycle_seconds = bicycle;
    journey.bike_share_uses = 1;
    journey.transfer_count = 0;
    journey.main_pattern_sequence.clear();
    journey
}

#[test]
fn bike_rides_from_nearby_stations_collapse_into_one_option() {
    let ranked = RankingPolicy::default().rank(vec![
        bike_only("station-a", 1_536, 221, 315),
        bike_only("station-b", 1_551, 163, 388),
        bike_only("station-c", 1_592, 290, 302),
    ]);

    assert_eq!(
        ranked
            .iter()
            .map(|value| value.stable_key.as_str())
            .collect::<Vec<_>>(),
        vec!["station-a"]
    );
}

#[test]
fn materially_shorter_bike_ride_remains_a_separate_option() {
    let policy = RankingPolicy::default();
    let threshold = policy.material_walking_difference_seconds;
    let ranked = policy.rank(vec![
        bike_only("long-ride", 1_500, 300, 900),
        bike_only("short-ride", 1_560, 400, 900 - threshold),
        bike_only("just-under", 1_520, 350, 900 - threshold + 1),
    ]);

    assert_eq!(
        ranked
            .iter()
            .map(|value| value.stable_key.as_str())
            .collect::<Vec<_>>(),
        vec!["long-ride", "short-ride"]
    );
}
