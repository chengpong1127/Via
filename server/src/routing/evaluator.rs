//! Bounded evaluation of retained Phase 1 corridors without topology search.

use std::{cmp::Ordering, collections::HashMap, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::domain::{
    ids::BoardingPointId,
    realtime::NormalizedRealtimeValue,
    transit_runtime::TransitRuntime,
    transit_timing::{EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot},
};

use super::{
    bike::{BikeMovementRealizations, CorridorMovement},
    model::{
        AccessPoint, EgressPoint, JourneyCandidate, JourneySegment, RoutingError, TransitQuery,
    },
    path::{BikeShortcut, JourneySkeleton, JourneyVariant},
    static_search::{CandidateCorridor, StaticCorridorStep, StaticTransitLeg},
    transfers::TransferShortcut,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phase2EvaluationStats {
    pub corridors_evaluated: usize,
    pub corridors_feasible: usize,
    pub corridor_states_evaluated: usize,
    pub transit_departure_lookups: usize,
    pub effective_trips_examined: usize,
    pub surface_transfers_evaluated: usize,
    pub walking_realizations_considered: usize,
    pub bike_realizations_considered: usize,
    pub bike_realizations_feasible: usize,
    pub bike_realizations_selected: usize,
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
struct EvaluationState {
    stop_id: BoardingPointId,
    ready_at: OffsetDateTime,
    transit_legs: usize,
    path: EvaluationPath,
}

#[derive(Clone)]
struct EvaluationPath {
    access: AccessPoint,
    segments: Vec<JourneySegment>,
    walking_seconds: u32,
    evidence: Vec<NormalizedRealtimeValue>,
    timing_bases: Vec<TimingBasis>,
    timing_reliability: TimingReliability,
    uncertainty_seconds: Option<u32>,
    bike_shortcuts: Vec<(usize, BikeShortcut)>,
}

impl EvaluationPath {
    fn from_access(access: AccessPoint) -> Self {
        Self {
            walking_seconds: access.walking_seconds,
            access,
            segments: Vec::new(),
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
            uncertainty_seconds: None,
            bike_shortcuts: Vec::new(),
        }
    }

    fn with_ride(&self, trip: &EffectiveTrip, board_index: usize, alight_index: usize) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::Transit {
            trip: trip.clone(),
            board_index,
            alight_index,
        });
        next.evidence.extend(trip.evidence.clone());
        next.timing_bases.push(trip.timing_basis);
        if trip.timing_reliability == TimingReliability::Fallback {
            next.timing_reliability = TimingReliability::Fallback;
        }
        next.uncertainty_seconds = match (next.uncertainty_seconds, trip.uncertainty_seconds) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (None, value) | (value, None) => value,
        };
        next
    }

    fn with_walking_transfer(&self, shortcut: &TransferShortcut) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: shortcut.clone(),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.duration_seconds);
        next
    }

    fn with_bike_transfer(&self, movement: &CorridorMovement, shortcut: &BikeShortcut) -> Self {
        let mut next = self.clone();
        let segment_index = next.segments.len();
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: TransferShortcut::new(
                movement.from.clone(),
                movement.to.clone(),
                shortcut.total_seconds,
            ),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.walk_to_pickup.duration_seconds)
            .saturating_add(shortcut.walk_from_dropoff.duration_seconds);
        next.bike_shortcuts.push((segment_index, shortcut.clone()));
        next
    }

    fn stable_signature(&self) -> String {
        let segments = self
            .segments
            .iter()
            .map(|segment| match segment {
                JourneySegment::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => {
                    format!("transit:{}:{board_index}:{alight_index}", trip.id.as_str())
                }
                JourneySegment::WalkingTransfer { shortcut } => format!(
                    "transfer:{}:{}:{}",
                    shortcut.from.as_str(),
                    shortcut.to.as_str(),
                    shortcut.duration_seconds
                ),
            })
            .collect::<Vec<_>>()
            .join("|");
        let bikes = self
            .bike_shortcuts
            .iter()
            .map(|(index, shortcut)| {
                format!(
                    "bike:{index}:{}:{}",
                    shortcut.pickup_station_id.as_str(),
                    shortcut.dropoff_station_id.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        format!("{segments}|{bikes}")
    }

    fn walking_seconds(&self) -> u32 {
        self.walking_seconds
    }

    fn bicycle_seconds(&self) -> u32 {
        self.bike_shortcuts
            .iter()
            .map(|(_, shortcut)| shortcut.bicycle_duration_seconds)
            .fold(0, u32::saturating_add)
    }

    fn same_timing_quality(&self, other: &Self) -> bool {
        self.timing_reliability == other.timing_reliability
            && self.timing_bases == other.timing_bases
            && self.evidence == other.evidence
    }
}

fn make_candidate(
    path: &EvaluationPath,
    egress: &EgressPoint,
    arrival_time: OffsetDateTime,
    transit_legs: usize,
) -> JourneyCandidate {
    JourneyCandidate {
        access: path.access.clone(),
        egress: egress.clone(),
        segments: path.segments.clone(),
        departure_time: path.access.departure_at,
        arrival_time,
        walking_seconds: path.walking_seconds.saturating_add(egress.walking_seconds),
        transfer_count: u32::try_from(transit_legs.saturating_sub(1)).unwrap_or(u32::MAX),
        uncertainty_seconds: path.uncertainty_seconds,
        evidence: path.evidence.clone(),
        timing_bases: path.timing_bases.clone(),
        timing_reliability: path.timing_reliability,
    }
}

struct EvaluatedCandidate {
    metrics: JourneyCandidate,
    variant: JourneyVariant,
    bike_share_uses: usize,
    bicycle_seconds: u32,
}

impl EvaluatedCandidate {
    fn dominates(&self, other: &Self) -> bool {
        self.bike_share_uses == other.bike_share_uses
            && self.metrics.timing_reliability == other.metrics.timing_reliability
            && self.metrics.timing_bases == other.metrics.timing_bases
            && self.metrics.evidence == other.metrics.evidence
            && self.bicycle_seconds <= other.bicycle_seconds
            && (self.metrics.dominates(&other.metrics)
                || (self.bicycle_seconds < other.bicycle_seconds
                    && self.metrics.arrival_time == other.metrics.arrival_time
                    && self.metrics.walking_seconds == other.metrics.walking_seconds
                    && self.metrics.transfer_count == other.metrics.transfer_count))
    }
}

fn make_variant(
    candidate: JourneyCandidate,
    bike_shortcuts: &[(usize, BikeShortcut)],
) -> Option<JourneyVariant> {
    let skeleton = JourneySkeleton::from_candidate(candidate).ok()?;
    let mut variant = JourneyVariant::baseline(skeleton);
    for (segment_index, shortcut) in bike_shortcuts {
        let movement_index = segment_index.saturating_add(1);
        let movement = variant.skeleton().movements().get(movement_index)?;
        let mut shortcut = shortcut.clone();
        shortcut.from_anchor = movement.from_anchor;
        shortcut.to_anchor = movement.to_anchor;
        variant = JourneyVariant::with_replacement(&variant, movement_index, shortcut)?;
    }
    Some(variant)
}

fn pareto_candidates(
    mut candidates: Vec<EvaluatedCandidate>,
    limit: usize,
) -> Vec<EvaluatedCandidate> {
    candidates.sort_by(|left, right| candidate_order(&left.metrics, &right.metrics));
    let mut pareto = Vec::new();
    for candidate in candidates {
        if pareto.iter().any(|existing: &EvaluatedCandidate| {
            existing.variant.signature() == candidate.variant.signature()
                || existing.dominates(&candidate)
        }) {
            continue;
        }
        pareto.retain(|existing: &EvaluatedCandidate| !candidate.dominates(existing));
        pareto.push(candidate);
    }
    pareto.truncate(limit.max(1));
    pareto
}

fn candidate_order(left: &JourneyCandidate, right: &JourneyCandidate) -> Ordering {
    left.arrival_time
        .cmp(&right.arrival_time)
        .then(left.transfer_count.cmp(&right.transfer_count))
        .then(left.walking_seconds.cmp(&right.walking_seconds))
        .then(left.signature().cmp(&right.signature()))
}

#[cfg(test)]
fn evaluate(
    network: &TransitRuntime,
    corridor: &CandidateCorridor,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<Vec<JourneyVariant>, RoutingError> {
    evaluate_with_stats(network, corridor, snapshot, query).map(|(items, _)| items)
}

#[cfg(test)]
fn evaluate_with_stats(
    network: &TransitRuntime,
    corridor: &CandidateCorridor,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    evaluate_with_realizations(
        network,
        corridor,
        snapshot,
        query,
        &BikeMovementRealizations::new(),
    )
}

pub(super) fn evaluate_with_realizations(
    network: &TransitRuntime,
    corridor: &CandidateCorridor,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
    bike_realizations: &BikeMovementRealizations,
) -> Result<(Vec<JourneyVariant>, Phase2EvaluationStats), RoutingError> {
    let started_at = Instant::now();
    if query.origin_access.is_empty() || query.destination_egress.is_empty() {
        return Err(RoutingError::InvalidQuery);
    }

    let egress_by_stop = index_egress(&query.destination_egress);
    let max_legs = query.max_transfers.saturating_add(1);
    let mut stats = Phase2EvaluationStats::default();
    let mut candidates = Vec::<EvaluatedCandidate>::new();

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
            .map(|access| EvaluationState {
                stop_id: access.stop_id.clone(),
                ready_at: access.available_at,
                transit_legs: 0,
                path: EvaluationPath::from_access(access),
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
                StaticCorridorStep::SurfaceTransfer {
                    from,
                    to,
                    walking_duration_seconds,
                } => {
                    stats.surface_transfers_evaluated += 1;
                    let movement = CorridorMovement {
                        from: from.clone(),
                        to: to.clone(),
                    };
                    let bike_options = bike_realizations
                        .get(&movement)
                        .map(Vec::as_slice)
                        .unwrap_or_default();
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
                        let ready_at = state.ready_at
                            + Duration::seconds(i64::from(*walking_duration_seconds));
                        if !within_query_bounds(&query, ready_at) {
                            continue;
                        }
                        let shortcut = TransferShortcut::new(
                            from.clone(),
                            to.clone(),
                            *walking_duration_seconds,
                        );
                        stats.walking_realizations_considered += 1;
                        next.push(EvaluationState {
                            stop_id: to.clone(),
                            ready_at,
                            transit_legs: state.transit_legs,
                            path: state.path.with_walking_transfer(&shortcut),
                        });
                        for bike in bike_options {
                            stats.bike_realizations_considered += 1;
                            if bike.total_seconds >= *walking_duration_seconds {
                                continue;
                            }
                            if charge_state(&mut stats, query.max_expansions) {
                                stats.expansion_limit_reached = true;
                                break 'corridors;
                            }
                            let ready_at =
                                state.ready_at + Duration::seconds(i64::from(bike.total_seconds));
                            if !within_query_bounds(&query, ready_at) {
                                continue;
                            }
                            stats.bike_realizations_feasible += 1;
                            next.push(EvaluationState {
                                stop_id: to.clone(),
                                ready_at,
                                transit_legs: state.transit_legs,
                                path: state.path.with_bike_transfer(&movement, bike),
                            });
                        }
                    }
                    states = prune_states(next, query.max_candidates);
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
                            if !valid_pattern_leg(network, corridor, leg, alternative) {
                                continue;
                            }
                            stats.transit_departure_lookups += 1;
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
                                let Some(arrival_at) = retained_alight_time(
                                    network,
                                    trip,
                                    alternative,
                                    state.ready_at,
                                ) else {
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
                                next.push(EvaluationState {
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
                    states = prune_states(next, query.max_candidates);
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
                let candidate = make_candidate(&state.path, egress, arrival_at, state.transit_legs);
                let Some(variant) = make_variant(candidate.clone(), &state.path.bike_shortcuts)
                else {
                    stats.malformed_trips += 1;
                    continue;
                };
                corridor_candidates.push(EvaluatedCandidate {
                    metrics: candidate,
                    bike_share_uses: variant
                        .path()
                        .iter()
                        .filter(|step| {
                            matches!(
                                step.movement,
                                super::path::JourneyPathMovement::BikeShare { .. }
                            )
                        })
                        .count(),
                    bicycle_seconds: state.path.bicycle_seconds(),
                    variant,
                });
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

    if stats.expansion_limit_reached {
        stats.phase2_latency_us = elapsed_micros(started_at);
        tracing::error!(
            target: "via_server::routing",
            corridors_evaluated = stats.corridors_evaluated,
            corridor_states_evaluated = stats.corridor_states_evaluated,
            max_expansions = query.max_expansions,
            phase2_latency_us = stats.phase2_latency_us,
            "Phase 2 hit its safety bound; rejecting the incomplete candidate set"
        );
        return Err(RoutingError::Phase2EvaluationLimit);
    }

    stats.candidates_before_pareto = candidates.len();
    let candidates = pareto_candidates(candidates, query.max_candidates);
    stats.final_candidates = candidates.len();
    stats.bike_realizations_selected = candidates
        .iter()
        .map(|candidate| {
            candidate
                .variant
                .path()
                .iter()
                .filter(|step| {
                    matches!(
                        step.movement,
                        super::path::JourneyPathMovement::BikeShare { .. }
                    )
                })
                .count()
        })
        .sum();
    stats.phase2_latency_us = elapsed_micros(started_at);
    tracing::debug!(
        target: "via_server::routing",
        corridors_evaluated = stats.corridors_evaluated,
        corridors_feasible = stats.corridors_feasible,
        corridor_states_evaluated = stats.corridor_states_evaluated,
        transit_departure_lookups = stats.transit_departure_lookups,
        effective_trips_examined = stats.effective_trips_examined,
        surface_transfers_evaluated = stats.surface_transfers_evaluated,
        walking_realizations_considered = stats.walking_realizations_considered,
        bike_realizations_considered = stats.bike_realizations_considered,
        bike_realizations_feasible = stats.bike_realizations_feasible,
        bike_realizations_selected = stats.bike_realizations_selected,
        infeasible_no_departure = stats.infeasible_no_departure,
        infeasible_transfer = stats.infeasible_transfer,
        infeasible_access = stats.infeasible_access,
        infeasible_constraints = stats.infeasible_constraints,
        malformed_trips = stats.malformed_trips,
        candidates_before_pareto = stats.candidates_before_pareto,
        final_candidates = stats.final_candidates,
        phase2_latency_us = stats.phase2_latency_us,
        expansion_limit_reached = stats.expansion_limit_reached,
        "Phase 2 corridor evaluation"
    );
    Ok((
        candidates
            .into_iter()
            .map(|candidate| candidate.variant)
            .collect(),
        stats,
    ))
}

fn charge_state(stats: &mut Phase2EvaluationStats, max_expansions: usize) -> bool {
    if stats.corridor_states_evaluated >= max_expansions {
        return true;
    }
    stats.corridor_states_evaluated += 1;
    false
}

fn valid_pattern_leg(
    network: &TransitRuntime,
    corridor: &CandidateCorridor,
    retained: &StaticTransitLeg,
    alternative: &StaticTransitLeg,
) -> bool {
    if !corridor.is_equivalent_leg(network, retained, alternative) {
        return false;
    }
    let Some(pattern) = network.pattern(&alternative.pattern_id) else {
        return false;
    };
    pattern.stops.get(alternative.board_index) == Some(&retained.board_stop_id)
        && pattern.stops.get(alternative.alight_index) == Some(&retained.alight_stop_id)
        && alternative.board_index < alternative.alight_index
}

fn retained_alight_time(
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
        || trip
            .stop_times
            .iter()
            .any(|time| time.arrival_at > time.departure_at)
        || trip
            .stop_times
            .windows(2)
            .any(|pair| pair[0].departure_at > pair[1].arrival_at)
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

fn prune_states(mut states: Vec<EvaluationState>, limit: usize) -> Vec<EvaluationState> {
    states.sort_by(|left, right| {
        left.ready_at
            .cmp(&right.ready_at)
            .then(
                left.path
                    .walking_seconds()
                    .cmp(&right.path.walking_seconds()),
            )
            .then(left.transit_legs.cmp(&right.transit_legs))
            .then(
                left.path
                    .stable_signature()
                    .cmp(&right.path.stable_signature()),
            )
    });
    let mut frontier = Vec::<EvaluationState>::new();
    for state in states {
        if frontier.iter().any(|known| {
            known.path.bike_shortcuts.len() == state.path.bike_shortcuts.len()
                && known.path.same_timing_quality(&state.path)
                && known.ready_at <= state.ready_at
                && known.path.walking_seconds() <= state.path.walking_seconds()
                && known.path.bicycle_seconds() <= state.path.bicycle_seconds()
                && known.transit_legs <= state.transit_legs
        }) {
            continue;
        }
        frontier.retain(|known| {
            !(state.path.bike_shortcuts.len() == known.path.bike_shortcuts.len()
                && state.path.same_timing_quality(&known.path)
                && state.ready_at <= known.ready_at
                && state.path.walking_seconds() <= known.path.walking_seconds()
                && state.path.bicycle_seconds() <= known.path.bicycle_seconds()
                && state.transit_legs <= known.transit_legs)
        });
        frontier.push(state);
    }
    frontier.truncate(limit.max(1));
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
            bikeshare::BikeWalkingAccess,
            ids::{BikeStationId, EffectiveTripId, TransitPatternId},
            transit::{TransferEdge, TransitMode, TransitNetwork, TransitPattern, TransitStop},
            transit_runtime::TransitRuntime,
            transit_timing::{EffectiveStopTime, TimingBasis, TimingReliability},
        },
        routing::{
            AccessPoint, EgressPoint,
            static_search::{StaticCorridor, generate},
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
    fn phase2_single_leg_route() {
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
        assert_eq!(
            candidates[0].evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:20 UTC)
        );
        assert_eq!(stats.corridors_feasible, 1);
    }

    #[test]
    fn phase2_multi_leg_route_keeps_explicit_surface_transfer() {
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
        assert_eq!(candidates[0].evaluate_timing().unwrap().walking_seconds, 30);
        assert!(matches!(
            candidates[0]
                .skeleton()
                .movements()
                .get(2)
                .map(|movement| &movement.kind),
            Some(super::super::path::JourneyMovementKind::Walking {
                kind: super::super::path::WalkingMovementKind::Transfer,
                ..
            })
        ));
    }

    #[test]
    fn faster_bike_surface_transfer_catches_the_next_transit_departure() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "transfer-from"]),
                pattern(
                    "second",
                    TransitMode::Metro,
                    &["transfer-to", "destination"],
                ),
            ],
            &["origin", "transfer-from", "transfer-to", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus, TransitMode::Metro], 1, 1);
        let corridor = CandidateCorridor::from_retained_for_test(
            &network,
            vec![StaticCorridor {
                access_stop_id: BoardingPointId::new("origin"),
                egress_stop_id: BoardingPointId::new("destination"),
                theoretical_duration_seconds: 0,
                cost_source: crate::routing::StaticCostSource::OptimisticEstimate,
                steps: vec![
                    static_leg("first", "origin", "transfer-from"),
                    StaticCorridorStep::SurfaceTransfer {
                        from: BoardingPointId::new("transfer-from"),
                        to: BoardingPointId::new("transfer-to"),
                        walking_duration_seconds: 8 * 60,
                    },
                    static_leg("second", "transfer-to", "destination"),
                ],
            }],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("first"),
            0,
            vec![trip(
                "first-trip",
                "first",
                &["origin", "transfer-from"],
                &[0, 0],
            )],
        );
        snapshot.insert(
            TransitPatternId::new("second"),
            0,
            vec![trip(
                "second-trip",
                "second",
                &["transfer-to", "destination"],
                &[6, 10],
            )],
        );
        let movement = CorridorMovement {
            from: BoardingPointId::new("transfer-from"),
            to: BoardingPointId::new("transfer-to"),
        };
        let bike = BikeShortcut::new(
            0,
            1,
            BikeStationId::new("pickup"),
            BikeStationId::new("dropoff"),
            BikeWalkingAccess::new(60, 50),
            crate::routing::BikeRide {
                duration_seconds: 200,
                distance_metres: 1_000,
            },
            BikeWalkingAccess::new(90, 50),
        )
        .unwrap();
        let realizations = HashMap::from([(movement, vec![bike])]);

        let (variants, stats) =
            evaluate_with_realizations(&network, &corridor, &snapshot, query, &realizations)
                .unwrap();

        assert_eq!(variants.len(), 1);
        let timing = variants[0].evaluate_timing().unwrap();
        assert_eq!(timing.arrival_at, datetime!(2026-09-21 10:10 UTC));
        assert_eq!(timing.bike_share_uses, 1);
        assert_eq!(timing.walking_seconds, 150);
        let bike_shortcut = variants[0]
            .path()
            .iter()
            .find_map(|step| match &step.movement {
                super::super::path::JourneyPathMovement::BikeShare { shortcut } => Some(shortcut),
                super::super::path::JourneyPathMovement::Baseline { .. } => None,
            })
            .unwrap();
        assert_eq!(bike_shortcut.walk_to_pickup.duration_seconds, 60);
        assert_eq!(bike_shortcut.walk_from_dropoff.duration_seconds, 90);
        let bike_step = variants[0]
            .path()
            .iter()
            .find(|step| {
                matches!(
                    step.movement,
                    super::super::path::JourneyPathMovement::BikeShare { .. }
                )
            })
            .unwrap();
        assert!(matches!(
            &variants[0].skeleton().anchors()[bike_step.from_anchor].kind,
            super::super::path::JourneyAnchorKind::BoardingPoint { id }
                if id == &BoardingPointId::new("transfer-from")
        ));
        assert!(matches!(
            &variants[0].skeleton().anchors()[bike_step.to_anchor].kind,
            super::super::path::JourneyAnchorKind::BoardingPoint { id }
                if id == &BoardingPointId::new("transfer-to")
        ));
        assert_eq!(stats.bike_realizations_selected, 1);
        assert!(variants[0].path().iter().any(|step| matches!(
            step.movement,
            super::super::path::JourneyPathMovement::BikeShare { .. }
        )));
    }

    #[test]
    fn phase2_rejects_corridor_without_a_departure_after_ready_at() {
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
    fn phase2_rejects_non_monotonic_effective_trip_times() {
        let network = network(
            vec![pattern(
                "bus",
                TransitMode::Bus,
                &["origin", "middle", "destination"],
            )],
            &["origin", "middle", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip(
                "malformed-trip",
                "bus",
                &["origin", "middle", "destination"],
                &[1, 5, 4],
            )],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.is_empty());
        assert!(stats.malformed_trips > 0);
    }

    #[test]
    fn phase2_later_departure_can_have_earlier_arrival() {
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
        assert_eq!(
            candidates[0].evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:15 UTC)
        );
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![TransitPatternId::new("bus")]
        );
        assert!(matches!(
            candidates[0]
                .skeleton()
                .movements()
                .get(1)
                .map(|movement| &movement.kind),
            Some(super::super::path::JourneyMovementKind::Transit { .. })
        ));
    }

    #[test]
    fn phase2_keeps_reliable_candidate_when_fallback_arrives_earlier() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut fallback = trip("a-fallback", "bus", &["origin", "destination"], &[1, 5]);
        fallback.timing_reliability = TimingReliability::Fallback;
        let reliable = trip("z-reliable", "bus", &["origin", "destination"], &[2, 6]);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(TransitPatternId::new("bus"), 0, vec![fallback, reliable]);

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.skeleton().movements().iter().any(|movement| {
                matches!(
                    &movement.kind,
                    super::super::path::JourneyMovementKind::Transit { trip, .. }
                        if trip.timing_reliability == TimingReliability::Reliable
                )
            })
        }));
    }

    #[test]
    fn phase2_state_pruning_keeps_reliability_alternatives_for_later_legs() {
        let network = network(
            vec![
                pattern("first", TransitMode::Bus, &["origin", "transfer"]),
                pattern("second", TransitMode::Bus, &["transfer", "destination"]),
            ],
            &["origin", "transfer", "destination"],
            Vec::new(),
        );
        let query = query(&[TransitMode::Bus], 1, 10);
        let corridor = candidate_corridor(&network, &query);
        let mut fallback = trip("a-fallback", "first", &["origin", "transfer"], &[1, 3]);
        fallback.timing_reliability = TimingReliability::Fallback;
        let reliable = trip("z-reliable", "first", &["origin", "transfer"], &[1, 3]);
        let second = trip(
            "second-trip",
            "second",
            &["transfer", "destination"],
            &[5, 10],
        );
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(TransitPatternId::new("first"), 0, vec![fallback, reliable]);
        snapshot.insert(TransitPatternId::new("second"), 0, vec![second]);

        let candidates = evaluate(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.skeleton().movements().iter().any(|movement| {
                matches!(
                    &movement.kind,
                    super::super::path::JourneyMovementKind::Transit { trip, .. }
                        if trip.id == EffectiveTripId::new("z-reliable")
                )
            })
        }));
    }

    #[test]
    fn phase2_respects_maximum_journey_duration_and_latest_arrival() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let corridor_query = query(&[TransitMode::Bus], 0, 10);
        let corridor = candidate_corridor(&network, &corridor_query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip(
                "late-trip",
                "bus",
                &["origin", "destination"],
                &[1, 20],
            )],
        );

        let short_duration_query = corridor_query.clone().with_limits(0, 16, 2_000, 3, 10 * 60);
        assert!(
            evaluate(&network, &corridor, &snapshot, short_duration_query)
                .unwrap()
                .is_empty()
        );

        let mut latest_arrival_query = corridor_query;
        latest_arrival_query.latest_arrival = Some(datetime!(2026-09-21 10:10 UTC));
        assert!(
            evaluate(&network, &corridor, &snapshot, latest_arrival_query)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn phase2_respects_candidate_limit_after_pareto_pruning() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let mut query = query(&[TransitMode::Bus], 0, 10);
        query.origin_access[0].walking_seconds = 200;
        query.origin_access.push(AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:03 UTC),
            100,
            0,
        ));
        query.origin_access.push(AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:05 UTC),
            0,
            0,
        ));
        query.max_candidates = 2;
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![
                trip("early-arrival", "bus", &["origin", "destination"], &[1, 5]),
                trip("middle-arrival", "bus", &["origin", "destination"], &[4, 8]),
                trip("late-arrival", "bus", &["origin", "destination"], &[6, 10]),
            ],
        );

        let (candidates, stats) =
            evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert_eq!(stats.final_candidates, 2);
        assert!(stats.candidates_before_pareto >= stats.final_candidates);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn phase2_expansion_limit_rejects_partial_candidates() {
        let network = network(
            vec![pattern("bus", TransitMode::Bus, &["origin", "destination"])],
            &["origin", "destination"],
            Vec::new(),
        );
        let mut query = query(&[TransitMode::Bus], 0, 10);
        query.origin_access.push(AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        ));
        query.max_expansions = 1;
        let corridor = candidate_corridor(&network, &query);
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert(
            TransitPatternId::new("bus"),
            0,
            vec![trip("bus-trip", "bus", &["origin", "destination"], &[1, 5])],
        );

        assert!(evaluate_with_stats(&network, &corridor, &snapshot, query).is_err());
    }

    #[test]
    fn phase2_applies_same_stop_transfer_time() {
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
        assert!(stats.transit_departure_lookups > 0);
    }

    #[test]
    fn explicit_surface_transfer_does_not_add_same_stop_penalty() {
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
        assert_eq!(
            candidates[0].evaluate_timing().unwrap().arrival_at,
            datetime!(2026-09-21 10:20 UTC)
        );
    }

    #[test]
    fn phase2_allows_equivalent_pattern_substitution() {
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
    fn phase2_never_combines_different_retained_corridors() {
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

        let (candidates, _) = evaluate_with_stats(&network, &corridor, &snapshot, query).unwrap();

        assert!(candidates.iter().all(|candidate| {
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
        assert!(!candidates.is_empty());
    }
}
