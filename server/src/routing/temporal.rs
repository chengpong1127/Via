//! Forward temporal search over an already-loaded timing snapshot.

use std::{collections::HashMap, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::domain::{
    ids::BoardingPointId,
    realtime::NormalizedRealtimeValue,
    transit::TransitNetwork,
    transit_timing::{EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot},
};

use super::{
    model::{
        AccessPoint, EgressPoint, JourneyCandidate, JourneySegment, RoutingError, TransitQuery,
    },
    reachability::DestinationReachability,
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
};

#[derive(Clone)]
struct SearchPath {
    access: AccessPoint,
    segments: Vec<JourneySegment>,
    walking_seconds: u32,
    evidence: Vec<NormalizedRealtimeValue>,
    timing_bases: Vec<TimingBasis>,
    timing_reliability: TimingReliability,
    uncertainty_seconds: Option<u32>,
}

impl SearchPath {
    fn from_access(access: AccessPoint) -> Self {
        Self {
            walking_seconds: access.walking_seconds,
            access,
            segments: Vec::new(),
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
            uncertainty_seconds: None,
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

    fn with_walking_transfer(&self, shortcut: &super::transfers::TransferShortcut) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: shortcut.clone(),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.duration_seconds);
        next
    }
}

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
}

pub fn search(
    network: &TransitNetwork,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    reachability: &DestinationReachability,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<Vec<JourneyCandidate>, RoutingError> {
    search_with_stats(network, topology, transfers, reachability, snapshot, query)
        .map(|(items, _)| items)
}

pub fn search_with_stats(
    network: &TransitNetwork,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    reachability: &DestinationReachability,
    snapshot: &TimingSnapshot,
    query: TransitQuery,
) -> Result<(Vec<JourneyCandidate>, TemporalSearchStats), RoutingError> {
    let started_at = Instant::now();
    if query.origin_access.is_empty() || query.destination_egress.is_empty() {
        return Err(RoutingError::InvalidQuery);
    }
    if query.max_expansions == 0 || reachability.is_empty() {
        return Ok((Vec::new(), TemporalSearchStats::default()));
    }

    let max_legs = query.max_transfers.saturating_add(1);
    let egress_by_stop = index_egress(&query.destination_egress);
    let mut stats = TemporalSearchStats {
        initial_states: query.origin_access.len(),
        ..TemporalSearchStats::default()
    };
    let mut frontier = query
        .origin_access
        .iter()
        .cloned()
        .map(|access| TimedState {
            stop_id: access.stop_id.clone(),
            ready_at: access.available_at,
            legs_used: 0,
            path: SearchPath::from_access(access),
        })
        .collect::<Vec<_>>();
    let mut best = HashMap::<(BoardingPointId, usize), OffsetDateTime>::new();
    for state in &frontier {
        insert_if_earlier(&mut best, state);
    }
    let mut candidates = Vec::new();

    while !frontier.is_empty() {
        let mut next_frontier = Vec::new();
        for state in frontier {
            if stats.states_expanded >= query.max_expansions {
                break;
            }
            stats.states_expanded += 1;
            for position in topology.occurrences_at(&state.stop_id) {
                let Some(remaining_legs) =
                    reachability.remaining_legs(&position.service_id, position.stop_index)
                else {
                    continue;
                };
                if state.legs_used.saturating_add(remaining_legs) > max_legs {
                    continue;
                }
                stats.departure_lookups += 1;
                for trip in snapshot.departures(
                    &position.service_id,
                    position.stop_index,
                    state.ready_at,
                    query.max_departures_per_query,
                ) {
                    stats.effective_trips += 1;
                    scan_trip(
                        network,
                        transfers,
                        &egress_by_stop,
                        &query,
                        &state,
                        position.stop_index,
                        trip,
                        &mut candidates,
                        &mut next_frontier,
                        &mut stats,
                    );
                }
            }
        }
        frontier = next_frontier
            .into_iter()
            .filter(|state| insert_if_earlier(&mut best, state))
            .collect();
    }

    let candidates = pareto_candidates(candidates, query.max_candidates);
    stats.final_candidates = candidates.len();
    tracing::debug!(
        target: "via_server::routing",
        initial_states = stats.initial_states,
        states_expanded = stats.states_expanded,
        departure_lookups = stats.departure_lookups,
        effective_trips = stats.effective_trips,
        transfer_expansions = stats.transfer_expansions,
        final_candidates = stats.final_candidates,
        latency_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        "temporal search"
    );
    Ok((candidates, stats))
}

#[allow(clippy::too_many_arguments)]
fn scan_trip(
    network: &TransitNetwork,
    transfers: &TransferShortcutIndex,
    egress_by_stop: &HashMap<BoardingPointId, Vec<EgressPoint>>,
    query: &TransitQuery,
    state: &TimedState,
    board_index: usize,
    trip: &EffectiveTrip,
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
            candidates.push(JourneyCandidate {
                access: ride_path.access.clone(),
                egress: egress.clone(),
                segments: ride_path.segments.clone(),
                departure_time: ride_path.access.departure_at,
                arrival_time,
                walking_seconds: ride_path
                    .walking_seconds
                    .saturating_add(egress.walking_seconds),
                transfer_count: u32::try_from(next_legs_used.saturating_sub(1)).unwrap_or(u32::MAX),
                uncertainty_seconds: ride_path.uncertainty_seconds,
                evidence: ride_path.evidence.clone(),
                timing_bases: ride_path.timing_bases.clone(),
                timing_reliability: ride_path.timing_reliability,
            });
        }

        if next_legs_used >= query.max_transfers.saturating_add(1) {
            continue;
        }
        next_frontier.push(TimedState {
            stop_id: stop_time.stop_id.clone(),
            ready_at: stop_time.arrival_at
                + Duration::seconds(i64::from(network.default_transfer_seconds)),
            legs_used: next_legs_used,
            path: ride_path.clone(),
        });
        stats.transfer_expansions += 1;
        for shortcut in transfers.transfers_from(&stop_time.stop_id) {
            next_frontier.push(TimedState {
                stop_id: shortcut.to.clone(),
                ready_at: stop_time.arrival_at
                    + Duration::seconds(i64::from(shortcut.duration_seconds)),
                legs_used: next_legs_used,
                path: ride_path.with_walking_transfer(shortcut),
            });
            stats.transfer_expansions += 1;
        }
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

fn pareto_candidates(mut candidates: Vec<JourneyCandidate>, limit: usize) -> Vec<JourneyCandidate> {
    candidates.sort_by(|left, right| {
        left.arrival_time
            .cmp(&right.arrival_time)
            .then(left.transfer_count.cmp(&right.transfer_count))
            .then(left.walking_seconds.cmp(&right.walking_seconds))
            .then(left.signature().cmp(&right.signature()))
    });
    let mut pareto = Vec::new();
    for candidate in candidates {
        if pareto.iter().any(|existing: &JourneyCandidate| {
            existing.dominates(&candidate) || existing.signature() == candidate.signature()
        }) {
            continue;
        }
        pareto.retain(|existing| !candidate.dominates(existing));
        pareto.push(candidate);
    }
    pareto.truncate(limit.max(1));
    pareto
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::{Time, macros::datetime};

    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::{
            ids::{EffectiveTripId, TransitServiceId},
            transit::{TransitMode, TransitService, TransitStop},
            transit_timing::EffectiveStopTime,
        },
        routing::{
            reachability::DestinationReachability, topology::TransitTopologyIndex,
            transfers::TransferShortcutIndex,
        },
    };

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
        let service = |value: &str, destination: &str| {
            TransitService::new(
                TransitServiceId::new(value),
                TransitMode::Bus,
                value,
                destination,
                vec![
                    BoardingPointId::new("origin"),
                    BoardingPointId::new(destination),
                ],
            )
        };
        let network = Arc::new(TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("origin"), stop("destination"), stop("elsewhere")],
            vec![
                service("useful", "destination"),
                service("unused-a", "elsewhere"),
                service("unused-b", "elsewhere"),
            ],
            Vec::new(),
        ));
        let topology = TransitTopologyIndex::new(&network);
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
        let reachability = DestinationReachability::build(
            &network,
            &topology,
            &transfers,
            &query.destination_egress,
            &query.allowed_transit_modes,
            1,
        );
        let trip = EffectiveTrip {
            id: EffectiveTripId::new("trip"),
            service_id: TransitServiceId::new("useful"),
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
        snapshot.insert(TransitServiceId::new("useful"), 0, vec![trip]);

        let (candidates, stats) = search_with_stats(
            &network,
            &topology,
            &transfers,
            &reachability,
            &snapshot,
            query,
        )
        .unwrap();
        assert_eq!(stats.departure_lookups, 1);
        assert_eq!(candidates.len(), 1);
    }
}
