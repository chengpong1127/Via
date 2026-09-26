//! Forward temporal search over an already-loaded timing snapshot.

use std::{collections::HashMap, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::domain::{
    ids::BoardingPointId,
    transit_runtime::TransitRuntime,
    transit_timing::{EffectiveTrip, TimingSnapshot},
};

use super::{
    model::{EgressPoint, JourneyCandidate, RoutingError, TransitQuery},
    phase2::{Phase2Path as SearchPath, make_candidate, pareto_candidates},
    static_search::CandidateCorridor,
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
};

#[derive(Clone)]
struct TimedState {
    stop_id: BoardingPointId,
    ready_at: OffsetDateTime,
    legs_used: usize,
    path: SearchPath,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TemporalSearchStats {
    pub initial_states: usize,
    pub states_expanded: usize,
    pub departure_lookups: usize,
    pub effective_trips: usize,
    pub transfer_expansions: usize,
    pub final_candidates: usize,
    pub positions_outside_corridor: usize,
    pub positions_over_leg_budget: usize,
    pub positions_without_departures: usize,
    pub candidates_before_pareto: usize,
    pub expansion_limit_reached: bool,
    pub phase2_latency_us: u64,
}

pub fn search(
    network: &TransitRuntime,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    corridor: &CandidateCorridor,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<Vec<JourneyCandidate>, RoutingError> {
    search_with_stats(network, topology, transfers, corridor, snapshot, query)
        .map(|(items, _)| items)
}

pub fn search_with_stats(
    network: &TransitRuntime,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    corridor: &CandidateCorridor,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<(Vec<JourneyCandidate>, TemporalSearchStats), RoutingError> {
    let started_at = Instant::now();
    if query.origin_access.is_empty() || query.destination_egress.is_empty() {
        return Err(RoutingError::InvalidQuery);
    }
    if query.max_expansions == 0 || corridor.is_empty() {
        return Ok((Vec::new(), TemporalSearchStats::default()));
    }

    let max_legs = query.max_transfers.saturating_add(1);
    let egress_by_stop = index_egress(&query.destination_egress);
    let mut stats = TemporalSearchStats {
        initial_states: query.origin_access.len(),
        ..TemporalSearchStats::default()
    };
    let mut best = HashMap::<(BoardingPointId, usize), OffsetDateTime>::new();
    let mut frontier = Vec::new();
    for access in &query.origin_access {
        let state = TimedState {
            stop_id: access.stop_id.clone(),
            ready_at: access.available_at,
            legs_used: 0,
            path: SearchPath::from_access(access.clone()),
        };
        if insert_if_earlier(&mut best, &state) {
            frontier.push(state.clone());
        }
        for shortcut in transfers.transfers_from(&access.stop_id) {
            if !has_reachable_boarding_opportunity(corridor, &shortcut.to, 0, max_legs) {
                continue;
            }
            let transferred = TimedState {
                stop_id: shortcut.to.clone(),
                ready_at: access.available_at
                    + Duration::seconds(i64::from(shortcut.duration_seconds)),
                legs_used: 0,
                path: state.path.with_walking_transfer(shortcut),
            };
            if insert_if_earlier(&mut best, &transferred) {
                frontier.push(transferred);
                stats.transfer_expansions += 1;
            }
        }
    }
    let mut candidates = Vec::new();

    while !frontier.is_empty() {
        let mut next_frontier = Vec::new();
        for state in frontier {
            if stats.states_expanded >= query.max_expansions {
                stats.expansion_limit_reached = true;
                break;
            }
            stats.states_expanded += 1;
            for position in topology.occurrences_at(&state.stop_id) {
                let Some(remaining_legs) =
                    corridor.remaining_legs(&position.pattern_id, position.stop_index)
                else {
                    stats.positions_outside_corridor += 1;
                    continue;
                };
                if state.legs_used.saturating_add(remaining_legs) > max_legs {
                    stats.positions_over_leg_budget += 1;
                    continue;
                }
                stats.departure_lookups += 1;
                let departures = snapshot.departures(
                    &position.pattern_id,
                    position.stop_index,
                    state.ready_at,
                    query.max_departures_per_query,
                );
                if departures.is_empty() {
                    stats.positions_without_departures += 1;
                }
                for trip in departures {
                    stats.effective_trips += 1;
                    scan_trip(
                        network,
                        transfers,
                        corridor,
                        &egress_by_stop,
                        &query,
                        &state,
                        position.stop_index,
                        trip,
                        max_legs,
                        &mut best,
                        &mut candidates,
                        &mut next_frontier,
                        &mut stats,
                    );
                }
            }
        }
        frontier = next_frontier;
    }

    stats.candidates_before_pareto = candidates.len();
    let candidates = pareto_candidates(candidates, query.max_candidates);
    stats.final_candidates = candidates.len();
    stats.phase2_latency_us = u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX);
    tracing::debug!(
        target: "via_server::routing",
        initial_states = stats.initial_states,
        states_expanded = stats.states_expanded,
        departure_lookups = stats.departure_lookups,
        effective_trips = stats.effective_trips,
        transfer_expansions = stats.transfer_expansions,
        final_candidates = stats.final_candidates,
        positions_outside_corridor = stats.positions_outside_corridor,
        positions_over_leg_budget = stats.positions_over_leg_budget,
        positions_without_departures = stats.positions_without_departures,
        candidates_before_pareto = stats.candidates_before_pareto,
        expansion_limit_reached = stats.expansion_limit_reached,
        phase2_latency_us = stats.phase2_latency_us,
        latency_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        "temporal search"
    );
    Ok((candidates, stats))
}

#[allow(clippy::too_many_arguments)]
fn scan_trip(
    network: &TransitRuntime,
    transfers: &TransferShortcutIndex,
    corridor: &CandidateCorridor,
    egress_by_stop: &HashMap<BoardingPointId, Vec<EgressPoint>>,
    query: &TransitQuery,
    state: &TimedState,
    board_index: usize,
    trip: &EffectiveTrip,
    max_legs: usize,
    best: &mut HashMap<(BoardingPointId, usize), OffsetDateTime>,
    candidates: &mut Vec<JourneyCandidate>,
    next_frontier: &mut Vec<TimedState>,
    stats: &mut TemporalSearchStats,
) {
    if trip.stop_times.get(board_index).map(|stop| &stop.stop_id) != Some(&state.stop_id) {
        return;
    }
    let next_legs_used = state.legs_used + 1;
    for alight_index in (board_index + 1)..trip.stop_times.len() {
        let stop_time = &trip.stop_times[alight_index];
        if stop_time.arrival_at - query.departure_time
            > Duration::seconds(i64::from(query.max_journey_duration_seconds))
        {
            continue;
        }
        let ride_path = state.path.with_ride(trip, board_index, alight_index);
        for egress in egress_by_stop.get(&stop_time.stop_id).into_iter().flatten() {
            let arrival_time =
                stop_time.arrival_at + Duration::seconds(i64::from(egress.walking_seconds));
            if query
                .latest_arrival
                .is_some_and(|latest| arrival_time > latest)
                || arrival_time - query.departure_time
                    > Duration::seconds(i64::from(query.max_journey_duration_seconds))
            {
                continue;
            }
            candidates.push(make_candidate(
                &ride_path,
                egress,
                arrival_time,
                next_legs_used,
            ));
        }

        for shortcut in transfers.transfers_from(&stop_time.stop_id) {
            if !corridor.allows_terminal_transfer(&shortcut.from, &shortcut.to) {
                continue;
            }
            let transfer_path = ride_path.with_walking_transfer(shortcut);
            let transfer_arrival =
                stop_time.arrival_at + Duration::seconds(i64::from(shortcut.duration_seconds));
            let mut reached_destination = false;
            for egress in egress_by_stop.get(&shortcut.to).into_iter().flatten() {
                let arrival_time =
                    transfer_arrival + Duration::seconds(i64::from(egress.walking_seconds));
                if query
                    .latest_arrival
                    .is_some_and(|latest| arrival_time > latest)
                    || arrival_time - query.departure_time
                        > Duration::seconds(i64::from(query.max_journey_duration_seconds))
                {
                    continue;
                }
                reached_destination = true;
                candidates.push(make_candidate(
                    &transfer_path,
                    egress,
                    arrival_time,
                    next_legs_used,
                ));
            }
            if reached_destination {
                stats.transfer_expansions += 1;
            }
        }

        if next_legs_used >= max_legs {
            continue;
        }
        if has_reachable_boarding_opportunity(
            corridor,
            &stop_time.stop_id,
            next_legs_used,
            max_legs,
        ) {
            push_if_earlier(
                best,
                next_frontier,
                TimedState {
                    stop_id: stop_time.stop_id.clone(),
                    ready_at: stop_time.arrival_at
                        + Duration::seconds(i64::from(network.same_stop_transfer_seconds())),
                    legs_used: next_legs_used,
                    path: ride_path.clone(),
                },
                stats,
            );
        }
        for shortcut in transfers.transfers_from(&stop_time.stop_id) {
            if !has_reachable_boarding_opportunity(corridor, &shortcut.to, next_legs_used, max_legs)
            {
                continue;
            }
            push_if_earlier(
                best,
                next_frontier,
                TimedState {
                    stop_id: shortcut.to.clone(),
                    ready_at: stop_time.arrival_at
                        + Duration::seconds(i64::from(shortcut.duration_seconds)),
                    legs_used: next_legs_used,
                    path: ride_path.with_walking_transfer(shortcut),
                },
                stats,
            );
        }
    }
}

fn has_reachable_boarding_opportunity(
    corridor: &CandidateCorridor,
    stop_id: &BoardingPointId,
    legs_used: usize,
    max_legs: usize,
) -> bool {
    corridor
        .boardings_at(stop_id)
        .iter()
        .any(|boarding| legs_used.saturating_add(boarding.min_remaining_legs) <= max_legs)
}

fn push_if_earlier(
    best: &mut HashMap<(BoardingPointId, usize), OffsetDateTime>,
    next_frontier: &mut Vec<TimedState>,
    state: TimedState,
    stats: &mut TemporalSearchStats,
) {
    if insert_if_earlier(best, &state) {
        next_frontier.push(state);
        stats.transfer_expansions += 1;
    }
}

fn insert_if_earlier(
    best: &mut HashMap<(BoardingPointId, usize), OffsetDateTime>,
    state: &TimedState,
) -> bool {
    let key = (state.stop_id.clone(), state.legs_used);
    if best
        .get(&key)
        .is_some_and(|ready_at| *ready_at <= state.ready_at)
    {
        return false;
    }
    best.insert(key, state.ready_at);
    true
}

fn index_egress(points: &[EgressPoint]) -> HashMap<BoardingPointId, Vec<EgressPoint>> {
    let mut by_stop = HashMap::<BoardingPointId, Vec<EgressPoint>>::new();
    for point in points {
        by_stop
            .entry(point.stop_id.clone())
            .or_default()
            .push(point.clone());
    }
    by_stop
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::{Time, macros::datetime};

    use super::*;
    use crate::{
        api::RequestCoordinate,
        data::static_transfers::enrich_network_with_static_transfers,
        domain::{
            ids::{EffectiveTripId, TransitPatternId},
            transit::{TransferEdge, TransitMode, TransitNetwork, TransitPattern, TransitStop},
            transit_timing::{EffectiveStopTime, TimingBasis, TimingReliability},
        },
        routing::{
            AccessPoint, EgressPoint, JourneySegment,
            static_search::{CandidateCorridor, generate as static_search},
            topology::TransitTopologyIndex,
            transfers::TransferShortcutIndex,
        },
        street::{Coordinate, FakeStreetRouter, Metres, Seconds, StreetTravelMode},
    };

    fn stop(value: &str) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(value),
            value,
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )
    }

    fn pattern(value: &str, pattern: &[&str]) -> TransitPattern {
        TransitPattern::fixture(
            TransitPatternId::new(value),
            TransitMode::Bus,
            value,
            pattern.last().copied().unwrap_or_default(),
            pattern
                .iter()
                .map(|stop| BoardingPointId::new(*stop))
                .collect(),
        )
    }

    fn trip(
        value: &str,
        pattern_id: &str,
        stops: &[&str],
        minute_offsets: &[i64],
    ) -> EffectiveTrip {
        assert_eq!(stops.len(), minute_offsets.len());
        let departure_time = datetime!(2026-08-31 10:00 UTC);
        EffectiveTrip {
            id: EffectiveTripId::new(value),
            pattern_id: TransitPatternId::new(pattern_id),
            scheduled_trip_id: None,
            stop_times: stops
                .iter()
                .zip(minute_offsets)
                .map(|(stop, minute_offset)| EffectiveStopTime {
                    stop_id: BoardingPointId::new(*stop),
                    arrival_at: departure_time + Duration::minutes(*minute_offset),
                    departure_at: departure_time + Duration::minutes(*minute_offset),
                })
                .collect(),
            timing_basis: TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        }
    }

    fn query(origin: &str, destination: &str, max_transfers: usize) -> TransitQuery {
        TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new(origin),
                datetime!(2026-08-31 10:00 UTC),
                datetime!(2026-08-31 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new(destination), 0, 0)],
            datetime!(2026-08-31 10:00 UTC),
        )
        .with_allowed_transit_modes([TransitMode::Bus])
        .with_limits(max_transfers, 16, 2_000, 3, 4 * 60 * 60)
    }

    fn candidate_corridor(
        network: &TransitNetwork,
        query: &TransitQuery,
    ) -> (
        TransitRuntime,
        TransitTopologyIndex,
        TransferShortcutIndex,
        CandidateCorridor,
    ) {
        let runtime = TransitRuntime::new(Arc::new(network.clone()));
        let topology = TransitTopologyIndex::new(runtime.data());
        let transfers = TransferShortcutIndex::from_network(runtime.data());
        let corridor = static_search(&runtime, &topology, &transfers, query).0;
        (runtime, topology, transfers, corridor)
    }

    #[test]
    fn later_arrival_to_same_stop_and_leg_count_is_rejected_before_push() {
        let network = TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("origin"), stop("mid"), stop("destination")],
            vec![
                pattern("early", &["origin", "mid", "destination"]),
                pattern("late", &["origin", "mid", "destination"]),
            ],
            Vec::new(),
        );
        let query = query("origin", "destination", 1);
        let (runtime, topology, transfers, corridor) = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("early"),
            0,
            vec![trip(
                "early-trip",
                "early",
                &["origin", "mid", "destination"],
                &[1, 10, 20],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("late"),
            0,
            vec![trip(
                "late-trip",
                "late",
                &["origin", "mid", "destination"],
                &[2, 20, 30],
            )],
        );

        let (candidates, stats) =
            search_with_stats(&runtime, &topology, &transfers, &corridor, &snapshot, query)
                .unwrap();

        assert_eq!(stats.transfer_expansions, 1);
        assert_eq!(stats.states_expanded, 2);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn transfer_to_structurally_dead_stop_is_never_pushed() {
        let network = TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![
                stop("origin"),
                stop("mid"),
                stop("destination"),
                stop("dead"),
            ],
            vec![pattern("main", &["origin", "mid", "destination"])],
            Vec::new(),
        )
        .with_transfer_edges(vec![TransferEdge::new(
            BoardingPointId::new("mid"),
            BoardingPointId::new("dead"),
            60,
        )]);
        let query = query("origin", "destination", 1);
        let (runtime, topology, transfers, corridor) = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("main"),
            0,
            vec![trip(
                "main-trip",
                "main",
                &["origin", "mid", "destination"],
                &[1, 10, 20],
            )],
        );

        let (candidates, stats) =
            search_with_stats(&runtime, &topology, &transfers, &corridor, &snapshot, query)
                .unwrap();

        assert_eq!(stats.transfer_expansions, 1);
        assert_eq!(stats.states_expanded, 2);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn generated_walking_transfer_reaches_phase_one_and_phase_two() {
        let mut network = TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![
                stop("origin"),
                stop("first"),
                stop("second"),
                stop("destination"),
            ],
            vec![
                pattern("first-service", &["origin", "first"]),
                pattern("second-service", &["second", "destination"]),
            ],
            Vec::new(),
        );
        let route_id = network.patterns[1].route_id.clone();
        network
            .routes
            .iter_mut()
            .find(|route| route.id == route_id)
            .unwrap()
            .mode = TransitMode::Metro;
        for (stop, latitude) in network.stops.iter_mut().zip([25.0, 25.01, 25.0105, 25.02]) {
            stop.coordinate.latitude = latitude;
        }
        let first = Coordinate::try_new(25.01, 121.5).unwrap();
        let second = Coordinate::try_new(25.0105, 121.5).unwrap();
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Walking,
                first,
                second,
                Seconds(60),
                Metres(65),
            )
            .build();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(enrich_network_with_static_transfers(&mut network, &street))
            .unwrap();
        let query = query("origin", "destination", 1)
            .with_allowed_transit_modes([TransitMode::Bus, TransitMode::Metro]);
        let (runtime, topology, transfers, corridor) = candidate_corridor(&network, &query);
        assert_eq!(
            corridor.retained()[0].pattern_sequence(),
            vec![
                TransitPatternId::new("first-service"),
                TransitPatternId::new("second-service")
            ]
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first-service"),
            0,
            vec![trip(
                "first-trip",
                "first-service",
                &["origin", "first"],
                &[1, 5],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("second-service"),
            0,
            vec![trip(
                "second-trip",
                "second-service",
                &["second", "destination"],
                &[7, 12],
            )],
        );

        let (candidates, stats) =
            search_with_stats(&runtime, &topology, &transfers, &corridor, &snapshot, query)
                .unwrap();

        assert_eq!(stats.transfer_expansions, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![
                TransitPatternId::new("first-service"),
                TransitPatternId::new("second-service")
            ]
        );
        assert_eq!(candidates[0].transfer_count, 1);
    }

    #[test]
    fn final_walking_transfer_to_egress_remains_searchable() {
        let network = TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("origin"), stop("alight"), stop("destination")],
            vec![pattern("main", &["origin", "alight"])],
            Vec::new(),
        )
        .with_transfer_edges(vec![TransferEdge::new(
            BoardingPointId::new("alight"),
            BoardingPointId::new("destination"),
            60,
        )]);
        let query = query("origin", "destination", 0);
        let (runtime, topology, transfers, corridor) = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("main"),
            0,
            vec![trip("main-trip", "main", &["origin", "alight"], &[1, 5])],
        );

        let (candidates, stats) =
            search_with_stats(&runtime, &topology, &transfers, &corridor, &snapshot, query)
                .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(stats.transfer_expansions, 1);
        assert!(matches!(
            candidates[0].segments.last(),
            Some(JourneySegment::WalkingTransfer { shortcut })
                if shortcut.to == BoardingPointId::new("destination")
        ));
        assert_eq!(candidates[0].arrival_time, datetime!(2026-08-31 10:06 UTC));
    }

    #[test]
    fn timing_is_looked_up_only_for_structurally_reachable_boardings() {
        let stop = |value: &str| {
            TransitStop::new(
                BoardingPointId::new(value),
                value,
                RequestCoordinate {
                    latitude: 25.0,
                    longitude: 121.5,
                },
            )
        };
        let pattern = |value: &str, destination: &str| {
            TransitPattern::fixture(
                TransitPatternId::new(value),
                TransitMode::Bus,
                value,
                destination,
                vec![
                    BoardingPointId::new("origin"),
                    BoardingPointId::new(destination),
                ],
            )
        };
        let network = Arc::new(TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("origin"), stop("destination"), stop("elsewhere")],
            vec![
                pattern("useful", "destination"),
                pattern("unused-a", "elsewhere"),
                pattern("unused-b", "elsewhere"),
            ],
            Vec::new(),
        ));
        let runtime = TransitRuntime::new(network.clone());
        let topology = TransitTopologyIndex::new(runtime.data());
        let transfers = TransferShortcutIndex::default();
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("origin"),
                datetime!(2026-08-31 10:00 UTC),
                datetime!(2026-08-31 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
            datetime!(2026-08-31 10:00 UTC),
        );
        let corridor = static_search(&runtime, &topology, &transfers, &query).0;
        let trip = EffectiveTrip {
            id: EffectiveTripId::new("trip"),
            pattern_id: TransitPatternId::new("useful"),
            scheduled_trip_id: None,
            stop_times: vec![
                EffectiveStopTime {
                    stop_id: BoardingPointId::new("origin"),
                    arrival_at: datetime!(2026-08-31 10:01 UTC),
                    departure_at: datetime!(2026-08-31 10:01 UTC),
                },
                EffectiveStopTime {
                    stop_id: BoardingPointId::new("destination"),
                    arrival_at: datetime!(2026-08-31 10:10 UTC),
                    departure_at: datetime!(2026-08-31 10:10 UTC),
                },
            ],
            timing_basis: TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        };
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(TransitPatternId::new("useful"), 0, vec![trip]);

        let (candidates, stats) =
            search_with_stats(&runtime, &topology, &transfers, &corridor, &snapshot, query)
                .unwrap();
        assert_eq!(stats.departure_lookups, 1);
        assert_eq!(candidates.len(), 1);
    }
}
