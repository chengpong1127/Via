//! Bounded evaluation of the retained Phase 1 corridors without topology search.

use std::{collections::HashMap, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::domain::{
    ids::BoardingPointId,
    transit_runtime::TransitRuntime,
    transit_timing::{EffectiveTrip, TimingSnapshot},
};

use super::{
    model::{EgressPoint, JourneyCandidate, RoutingError, TransitQuery},
    phase2::{Phase2Path, make_candidate, pareto_candidates},
    static_search::{CandidateCorridor, StaticCorridorStep, StaticTransitLeg},
    transfers::TransferShortcut,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixedCorridorStats {
    pub corridors_evaluated: usize,
    pub corridors_feasible: usize,
    pub corridor_states_evaluated: usize,
    pub departure_lookups: usize,
    pub effective_trips_examined: usize,
    pub infeasible_no_departure: usize,
    pub infeasible_transfer: usize,
    pub infeasible_access: usize,
    pub infeasible_constraints: usize,
    pub malformed_trips: usize,
    pub candidates_before_pareto: usize,
    pub final_candidates: usize,
    pub phase2_latency_us: u64,
    pub expansion_limit_reached: bool,
}

#[derive(Clone)]
struct FixedState {
    stop_id: BoardingPointId,
    ready_at: OffsetDateTime,
    transit_legs: usize,
    path: Phase2Path,
}

#[cfg(test)]
fn evaluate(
    network: &TransitRuntime,
    corridor: &CandidateCorridor,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<Vec<JourneyCandidate>, RoutingError> {
    evaluate_with_stats(network, corridor, snapshot, query).map(|(items, _)| items)
}

pub(super) fn evaluate_with_stats(
    network: &TransitRuntime,
    corridor: &CandidateCorridor,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<(Vec<JourneyCandidate>, FixedCorridorStats), RoutingError> {
    let started_at = Instant::now();
    if query.origin_access.is_empty() || query.destination_egress.is_empty() {
        return Err(RoutingError::InvalidQuery);
    }

    let egress_by_stop = index_egress(&query.destination_egress);
    let max_legs = query.max_transfers.saturating_add(1);
    let mut stats = FixedCorridorStats::default();
    let mut candidates = Vec::new();

    'corridors: for retained in corridor.retained() {
        stats.corridors_evaluated += 1;
        let corridor_legs = retained.transit_legs().count();
        if corridor_legs == 0 || corridor_legs > max_legs {
            stats.infeasible_transfer += 1;
            continue;
        }
        let mut states = query
            .origin_access
            .iter()
            .filter(|access| access.stop_id == retained.access_stop_id)
            .cloned()
            .map(|access| FixedState {
                stop_id: access.stop_id.clone(),
                ready_at: access.available_at,
                transit_legs: 0,
                path: Phase2Path::from_access(access),
            })
            .collect::<Vec<_>>();
        if states.is_empty() {
            stats.infeasible_access += 1;
            continue;
        }

        let mut saw_missing_departure = false;
        let mut transfer_mismatch = false;
        for (step_index, step) in retained.steps.iter().enumerate() {
            if states.is_empty() {
                break;
            }
            match step {
                StaticCorridorStep::WalkingTransfer {
                    from,
                    to,
                    duration_seconds,
                } => {
                    let mut next = Vec::with_capacity(states.len());
                    for state in states {
                        if charge_state(&mut stats, query.max_expansions) {
                            stats.expansion_limit_reached = true;
                            break 'corridors;
                        }
                        if state.stop_id != *from {
                            transfer_mismatch = true;
                            continue;
                        }
                        let ready_at =
                            state.ready_at + Duration::seconds(i64::from(*duration_seconds));
                        if !within_query_bounds(&query, ready_at) {
                            continue;
                        }
                        let shortcut =
                            TransferShortcut::new(from.clone(), to.clone(), *duration_seconds);
                        next.push(FixedState {
                            stop_id: to.clone(),
                            ready_at,
                            transit_legs: state.transit_legs,
                            path: state.path.with_walking_transfer(&shortcut),
                        });
                    }
                    states = prune_states(next);
                }
                StaticCorridorStep::Transit(leg) => {
                    let alternatives = corridor.equivalent_legs_for(network, leg);
                    let mut next = Vec::new();
                    for state in states {
                        if state.stop_id != leg.board_stop_id {
                            transfer_mismatch = true;
                            continue;
                        }
                        for alternative in &alternatives {
                            if charge_state(&mut stats, query.max_expansions) {
                                stats.expansion_limit_reached = true;
                                break 'corridors;
                            }
                            if !valid_pattern_leg(network, leg, alternative) {
                                continue;
                            }
                            stats.departure_lookups += 1;
                            let departures = snapshot.departures(
                                &alternative.pattern_id,
                                alternative.board_index,
                                state.ready_at,
                                query.max_departures_per_query,
                            );
                            if departures.is_empty() {
                                saw_missing_departure = true;
                            }
                            stats.effective_trips_examined += departures.len();
                            for trip in departures {
                                let Some(arrival_at) =
                                    fixed_alight_time(network, trip, alternative, state.ready_at)
                                else {
                                    stats.malformed_trips += 1;
                                    continue;
                                };
                                if !within_query_bounds(&query, arrival_at)
                                    || query
                                        .latest_arrival
                                        .is_some_and(|latest| arrival_at > latest)
                                {
                                    continue;
                                }
                                let next_step_is_transit = matches!(
                                    retained.steps.get(step_index + 1),
                                    Some(StaticCorridorStep::Transit(_))
                                );
                                let ready_at = if next_step_is_transit {
                                    arrival_at
                                        + Duration::seconds(i64::from(
                                            network.same_stop_transfer_seconds(),
                                        ))
                                } else {
                                    arrival_at
                                };
                                if !within_query_bounds(&query, ready_at) {
                                    continue;
                                }
                                next.push(FixedState {
                                    stop_id: alternative.alight_stop_id.clone(),
                                    ready_at,
                                    transit_legs: state.transit_legs + 1,
                                    path: state.path.with_ride(
                                        trip,
                                        alternative.board_index,
                                        alternative.alight_index,
                                    ),
                                });
                            }
                        }
                    }
                    states = prune_states(next);
                }
            }
        }

        let mut corridor_candidates = Vec::new();
        for state in states {
            if state.stop_id != retained.egress_stop_id {
                transfer_mismatch = true;
                continue;
            }
            for egress in egress_by_stop
                .get(&retained.egress_stop_id)
                .into_iter()
                .flatten()
            {
                let arrival_at =
                    state.ready_at + Duration::seconds(i64::from(egress.walking_seconds));
                if !within_query_bounds(&query, arrival_at)
                    || query
                        .latest_arrival
                        .is_some_and(|latest| arrival_at > latest)
                {
                    continue;
                }
                corridor_candidates.push(make_candidate(
                    &state.path,
                    egress,
                    arrival_at,
                    state.transit_legs,
                ));
            }
        }
        if corridor_candidates.is_empty() {
            if transfer_mismatch {
                stats.infeasible_transfer += 1;
            } else if saw_missing_departure {
                stats.infeasible_no_departure += 1;
            } else {
                stats.infeasible_constraints += 1;
            }
        } else {
            stats.corridors_feasible += 1;
            candidates.extend(corridor_candidates);
        }
    }

    stats.candidates_before_pareto = candidates.len();
    let candidates = pareto_candidates(candidates, query.max_candidates);
    stats.final_candidates = candidates.len();
    stats.phase2_latency_us = elapsed_micros(started_at);
    tracing::debug!(
        target: "via_server::routing",
        corridors_evaluated = stats.corridors_evaluated,
        corridors_feasible = stats.corridors_feasible,
        corridor_states_evaluated = stats.corridor_states_evaluated,
        departure_lookups = stats.departure_lookups,
        effective_trips_examined = stats.effective_trips_examined,
        infeasible_no_departure = stats.infeasible_no_departure,
        infeasible_transfer = stats.infeasible_transfer,
        infeasible_access = stats.infeasible_access,
        infeasible_constraints = stats.infeasible_constraints,
        malformed_trips = stats.malformed_trips,
        candidates_before_pareto = stats.candidates_before_pareto,
        final_candidates = stats.final_candidates,
        phase2_latency_us = stats.phase2_latency_us,
        expansion_limit_reached = stats.expansion_limit_reached,
        "fixed corridor evaluation"
    );
    Ok((candidates, stats))
}

fn charge_state(stats: &mut FixedCorridorStats, max_expansions: usize) -> bool {
    if stats.corridor_states_evaluated >= max_expansions {
        return true;
    }
    stats.corridor_states_evaluated += 1;
    false
}

fn valid_pattern_leg(
    network: &TransitRuntime,
    retained: &StaticTransitLeg,
    alternative: &StaticTransitLeg,
) -> bool {
    if alternative.board_stop_id != retained.board_stop_id
        || alternative.alight_stop_id != retained.alight_stop_id
    {
        return false;
    }
    let Some(pattern) = network.pattern(&alternative.pattern_id) else {
        return false;
    };
    pattern.stops.get(alternative.board_index) == Some(&retained.board_stop_id)
        && pattern.stops.get(alternative.alight_index) == Some(&retained.alight_stop_id)
        && alternative.board_index < alternative.alight_index
}

fn fixed_alight_time(
    network: &TransitRuntime,
    trip: &EffectiveTrip,
    leg: &StaticTransitLeg,
    ready_at: OffsetDateTime,
) -> Option<OffsetDateTime> {
    if trip.pattern_id != leg.pattern_id || leg.board_index >= leg.alight_index {
        return None;
    }
    let pattern = network.pattern(&leg.pattern_id)?;
    if trip.stop_times.len() != pattern.stops.len()
        || trip
            .stop_times
            .iter()
            .zip(&pattern.stops)
            .any(|(time, stop_id)| &time.stop_id != stop_id)
    {
        return None;
    }
    let board = trip.stop_times.get(leg.board_index)?;
    let alight = trip.stop_times.get(leg.alight_index)?;
    if board.stop_id != leg.board_stop_id
        || alight.stop_id != leg.alight_stop_id
        || board.departure_at < ready_at
        || alight.arrival_at < board.departure_at
    {
        return None;
    }
    Some(alight.arrival_at)
}

fn within_query_bounds(query: &TransitQuery, time: OffsetDateTime) -> bool {
    time >= query.departure_time
        && time - query.departure_time
            <= Duration::seconds(i64::from(query.max_journey_duration_seconds))
}

fn prune_states(mut states: Vec<FixedState>) -> Vec<FixedState> {
    states.sort_by(|left, right| {
        left.ready_at.cmp(&right.ready_at).then(
            left.path
                .walking_seconds()
                .cmp(&right.path.walking_seconds()),
        )
    });
    let mut frontier = Vec::<FixedState>::new();
    for state in states {
        if frontier.iter().any(|known| {
            known.ready_at <= state.ready_at
                && known.path.walking_seconds() <= state.path.walking_seconds()
        }) {
            continue;
        }
        frontier.retain(|known| {
            !(state.ready_at <= known.ready_at
                && state.path.walking_seconds() <= known.path.walking_seconds())
        });
        frontier.push(state);
    }
    frontier
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

fn elapsed_micros(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::{Duration, Time, macros::datetime};

    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::{
            ids::{EffectiveTripId, TransitPatternId},
            transit::{TransferEdge, TransitMode, TransitNetwork, TransitPattern, TransitStop},
            transit_runtime::TransitRuntime,
            transit_timing::{EffectiveStopTime, TimingBasis, TimingReliability},
        },
        routing::{
            AccessPoint, EgressPoint,
            static_search::{StaticCorridor, generate},
            temporal,
            topology::TransitTopologyIndex,
            transfers::TransferShortcutIndex,
        },
    };

    fn stop(id: &str) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(id),
            id,
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )
    }

    fn pattern(id: &str, mode: TransitMode, stops: &[&str]) -> TransitPattern {
        TransitPattern::fixture(
            TransitPatternId::new(id),
            mode,
            id,
            stops.last().copied().unwrap_or_default(),
            stops
                .iter()
                .map(|stop| BoardingPointId::new(*stop))
                .collect(),
        )
    }

    fn network(
        patterns: Vec<TransitPattern>,
        stop_ids: &[&str],
        transfers: Vec<TransferEdge>,
    ) -> Arc<TransitRuntime> {
        Arc::new(TransitRuntime::new(Arc::new(
            TransitNetwork::fixture(
                Time::from_hms(4, 0, 0).unwrap(),
                stop_ids.iter().map(|id| stop(id)).collect(),
                patterns,
                Vec::new(),
            )
            .with_transfer_edges(transfers),
        )))
    }

    fn query(modes: &[TransitMode], max_transfers: usize, max_corridors: usize) -> TransitQuery {
        TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new("origin"),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes(modes.iter().copied())
        .with_limits(max_transfers, 16, 2_000, 3, 4 * 60 * 60)
        .with_max_static_corridors(max_corridors)
    }

    fn candidate_corridor(network: &TransitRuntime, query: &TransitQuery) -> CandidateCorridor {
        let topology = TransitTopologyIndex::new(network.data());
        let transfers = TransferShortcutIndex::from_network(network.data());
        generate(network, &topology, &transfers, query).0
    }

    fn trip(id: &str, pattern_id: &str, stops: &[&str], minute_offsets: &[i64]) -> EffectiveTrip {
        assert_eq!(stops.len(), minute_offsets.len());
        EffectiveTrip {
            id: EffectiveTripId::new(id),
            pattern_id: TransitPatternId::new(pattern_id),
            scheduled_trip_id: None,
            stop_times: stops
                .iter()
                .zip(minute_offsets)
                .map(|(stop, minute)| EffectiveStopTime {
                    stop_id: BoardingPointId::new(*stop),
                    arrival_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(*minute),
                    departure_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(*minute),
                })
                .collect(),
            timing_basis: TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        }
    }

    fn static_leg(pattern_id: &str, board: &str, alight: &str) -> StaticCorridorStep {
        StaticCorridorStep::Transit(StaticTransitLeg {
            pattern_id: TransitPatternId::new(pattern_id),
            board_stop_id: BoardingPointId::new(board),
            board_index: 0,
            alight_stop_id: BoardingPointId::new(alight),
            alight_index: 1,
            cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
        })
    }

    #[test]
    fn fixed_single_leg_route() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip(
                "bus-trip",
                "bus",
                &["origin", "destination"],
                &[1, 20],
            )],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].arrival_time, datetime!(2026-09-21 10:20 UTC));
        assert_eq!(stats.corridors_feasible, 1);
    }

    #[test]
    fn fixed_multi_leg_route_keeps_explicit_walking_transfer() {
        let network = network(
            vec![
                pattern("bus", TransitMode::Bus, &["origin", "bus-end"]),
                pattern("metro", TransitMode::Metro, &["metro-start", "destination"]),
            ],
            &["origin", "bus-end", "metro-start", "destination"],
            vec![TransferEdge::new(
                BoardingPointId::new("bus-end"),
                BoardingPointId::new("metro-start"),
                30,
            )],
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip("bus-trip", "bus", &["origin", "bus-end"], &[1, 9])],
        );
        snapshot.insert(
            TransitPatternId::new("metro"),
            0,
            vec![trip(
                "metro-trip",
                "metro",
                &["metro-start", "destination"],
                &[10, 20],
            )],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].transfer_count, 1);
        assert_eq!(candidates[0].walking_seconds, 30);
        assert!(matches!(
            candidates[0].segments.get(1),
            Some(super::super::model::JourneySegment::WalkingTransfer { shortcut })
                if shortcut.from == BoardingPointId::new("bus-end")
                    && shortcut.to == BoardingPointId::new("metro-start")
        ));
    }

    #[test]
    fn fixed_rejects_corridor_without_a_departure_after_ready_at() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let snapshot = TimingSnapshot::default();

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.is_empty());
        assert_eq!(stats.infeasible_no_departure, 1);
    }

    #[test]
    fn fixed_checks_multiple_departures_and_chooses_earlier_arrival() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![
                trip("early-slow", "bus", &["origin", "destination"], &[1, 30]),
                trip("late-fast", "bus", &["origin", "destination"], &[4, 15]),
            ],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].arrival_time, datetime!(2026-09-21 10:15 UTC));
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![TransitPatternId::new("bus")]
        );
        assert_eq!(
            candidates[0].segments[0].pattern_id(),
            Some(&TransitPatternId::new("bus"))
        );
    }

    #[test]
    fn fixed_applies_same_stop_transfer_time() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "middle"]),
                pattern("second", TransitMode::Metro, &["middle", "destination"]),
            ],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip("first-trip", "first", &["origin", "middle"], &[1, 10])],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip(
                "too-soon",
                "second",
                &["middle", "destination"],
                &[11, 20],
            )],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.is_empty());
        assert!(network.same_stop_transfer_seconds() >= 60);
        assert_eq!(stats.corridors_feasible, 0);
        assert!(stats.departure_lookups > 0);
    }

    #[test]
    fn explicit_walking_transfer_does_not_add_same_stop_penalty() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "bus-end"]),
                pattern(
                    "second",
                    TransitMode::Metro,
                    &["metro-start", "destination"],
                ),
            ],
            &["origin", "bus-end", "metro-start", "destination"],
            vec![TransferEdge::new(
                BoardingPointId::new("bus-end"),
                BoardingPointId::new("metro-start"),
                30,
            )],
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip("first-trip", "first", &["origin", "bus-end"], &[1, 9])],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip(
                "second-trip",
                "second",
                &["metro-start", "destination"],
                &[10, 20],
            )],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].arrival_time, datetime!(2026-09-21 10:20 UTC));
    }

    #[test]
    fn fixed_allows_equivalent_pattern_substitution() {
        let network = network(
            vec![
                pattern("pattern-a", TransitMode::Bus, &["origin", "destination"]),
                pattern("pattern-b", TransitMode::Bus, &["origin", "destination"]),
            ],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 1);
        let corridor = candidate_corridor(&network, &query);
        let retained_leg = corridor.retained()[0].transit_legs().next().unwrap();
        let alternatives = corridor.equivalent_legs_for(&network, retained_leg);
        assert_eq!(alternatives.len(), 2);
        let alternative = alternatives
            .iter()
            .find(|leg| leg.pattern_id != retained_leg.pattern_id)
            .unwrap();
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            alternative.pattern_id.clone(),
            alternative.board_index,
            vec![trip(
                "equivalent-trip",
                alternative.pattern_id.as_str(),
                &["origin", "destination"],
                &[1, 10],
            )],
        );

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![alternative.pattern_id.clone()]
        );
    }

    #[test]
    fn fixed_does_not_recombine_retained_corridors_and_compare_detects_recombination() {
        let network = network(
            vec![
                pattern("a-bus-prefix", TransitMode::Bus, &["origin", "middle"]),
                pattern("b-bus-suffix", TransitMode::Bus, &["middle", "destination"]),
                pattern("c-metro-prefix", TransitMode::Metro, &["origin", "middle"]),
                pattern(
                    "d-metro-suffix",
                    TransitMode::Metro,
                    &["middle", "destination"],
                ),
            ],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 2);
        let retained = vec![
            StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 1,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("a-bus-prefix", "origin", "middle"),
                    static_leg("b-bus-suffix", "middle", "destination"),
                ],
            },
            StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 2,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("c-metro-prefix", "origin", "middle"),
                    static_leg("d-metro-suffix", "middle", "destination"),
                ],
            },
        ];
        let corridor = CandidateCorridor::from_retained_for_test(&network, retained);
        let topology = TransitTopologyIndex::new(network.data());
        let transfers = TransferShortcutIndex::from_network(network.data());
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("a-bus-prefix"),
            0,
            vec![trip(
                "a-prefix-trip",
                "a-bus-prefix",
                &["origin", "middle"],
                &[1, 5],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("b-bus-suffix"),
            0,
            vec![trip(
                "b-suffix-trip",
                "b-bus-suffix",
                &["middle", "destination"],
                &[16, 30],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("c-metro-prefix"),
            0,
            vec![trip(
                "z-prefix-trip",
                "c-metro-prefix",
                &["origin", "middle"],
                &[1, 5],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("d-metro-suffix"),
            0,
            vec![trip(
                "d-suffix-trip",
                "d-metro-suffix",
                &["middle", "destination"],
                &[10, 18],
            )],
        );

        let (fixed_candidates, fixed_stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query.clone()).unwrap();
        let (union_candidates, union_stats) = temporal::search_with_stats(
            &network, &topology, &transfers, &corridor, &snapshot, query,
        )
        .unwrap();
        let comparison = super::super::phase2::comparison_stats(
            &network,
            &corridor,
            super::super::phase2::Phase2Run {
                candidates: &union_candidates,
                latency_us: union_stats.phase2_latency_us,
                departure_lookups: union_stats.departure_lookups,
                states_evaluated: union_stats.states_expanded,
            },
            super::super::phase2::Phase2Run {
                candidates: &fixed_candidates,
                latency_us: fixed_stats.phase2_latency_us,
                departure_lookups: fixed_stats.departure_lookups,
                states_evaluated: fixed_stats.corridor_states_evaluated,
            },
        );

        assert!(fixed_candidates.iter().all(|candidate| {
            let sequence = candidate.transit_pattern_sequence();
            sequence
                == [
                    TransitPatternId::new("a-bus-prefix"),
                    TransitPatternId::new("b-bus-suffix"),
                ]
                || sequence
                    == [
                        TransitPatternId::new("c-metro-prefix"),
                        TransitPatternId::new("d-metro-suffix"),
                    ]
        }));
        assert_eq!(
            union_candidates[0].transit_pattern_sequence(),
            vec![
                TransitPatternId::new("a-bus-prefix"),
                TransitPatternId::new("d-metro-suffix")
            ]
        );
        assert!(comparison.recombination_used);
        assert!(!comparison.union_best_in_retained_topk_structural);
    }
}
