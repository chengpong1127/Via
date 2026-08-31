use via_server::{
    live_status::FreshnessState,
    ranking::{
        ActionState, BikeAvailability, CandidateJourney, JourneyClass, JourneyLabel, RankingPolicy,
        TransferEvidence, TransferKind,
    },
};

fn candidate(key: &str, arrival: i64) -> CandidateJourney {
    CandidateJourney {
        stable_key: key.into(),
        departure_epoch_seconds: 1_000,
        arrival_epoch_seconds: arrival,
        walking_seconds: 300,
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
                3_000
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
            option.bike_availability = Some(BikeAvailability {
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
    assert_eq!(ranked[0].stable_key, "bike-3");
    assert_eq!(ranked[0].class, JourneyClass::Reliable);
    assert_eq!(ranked[1].class, JourneyClass::Feasible);
    assert_eq!(ranked[2].class, JourneyClass::Feasible);
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
