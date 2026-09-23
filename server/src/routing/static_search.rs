//! Bounded static Top-K corridor generation.
//!
//! This phase uses immutable transit data only. Exact scheduled or normalized
//! Metro travel times are preferred. Services without that timing (normally
//! Bus) remain searchable through geometry-derived optimistic in-vehicle
//! estimates with zero wait: Bus 60 km/h, Metro 100 km/h, Rail 160 km/h, and
//! High Speed Rail 350 km/h. These values rank candidate corridors; the
//! request-local timing snapshot remains authoritative for feasibility.

use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap, HashSet},
    time::Instant,
};

use crate::domain::{
    ids::{BoardingPointId, TransitServiceId},
    location::RequestCoordinate,
    transit::{TransitMode, TransitNetwork},
};

use super::{
    model::TransitQuery,
    topology::{ServicePosition, TransitTopologyIndex},
    transfers::TransferShortcutIndex,
};

const BUS_OPTIMISTIC_KMH: f64 = 60.0;
const METRO_OPTIMISTIC_KMH: f64 = 100.0;
const RAIL_OPTIMISTIC_KMH: f64 = 160.0;
const HIGH_SPEED_RAIL_OPTIMISTIC_KMH: f64 = 350.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticCostSource {
    ExactSchedule,
    OptimisticEstimate,
    Mixed,
}

impl StaticCostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactSchedule => "exact_static_schedule",
            Self::OptimisticEstimate => "optimistic_estimate",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticTransitLeg {
    pub service_id: TransitServiceId,
    pub board_stop_id: BoardingPointId,
    pub board_index: usize,
    pub alight_stop_id: BoardingPointId,
    pub alight_index: usize,
    pub cost_source: StaticCostSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StaticCorridorStep {
    Transit(StaticTransitLeg),
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
        duration_seconds: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticCorridor {
    pub access_stop_id: BoardingPointId,
    pub egress_stop_id: BoardingPointId,
    pub theoretical_duration_seconds: u32,
    pub cost_source: StaticCostSource,
    pub steps: Vec<StaticCorridorStep>,
}

impl StaticCorridor {
    pub fn transit_legs(&self) -> impl Iterator<Item = &StaticTransitLeg> {
        self.steps.iter().filter_map(|step| match step {
            StaticCorridorStep::Transit(leg) => Some(leg),
            StaticCorridorStep::WalkingTransfer { .. } => None,
        })
    }

    pub fn service_sequence(&self) -> Vec<TransitServiceId> {
        self.transit_legs()
            .map(|leg| leg.service_id.clone())
            .collect()
    }

    fn signature(&self) -> CorridorSignature {
        CorridorSignature {
            access_stop_id: self.access_stop_id.clone(),
            steps: structural_steps(&self.steps),
            egress_stop_id: self.egress_stop_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CorridorSignature {
    access_stop_id: BoardingPointId,
    steps: Vec<StructuralStep>,
    egress_stop_id: BoardingPointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PartialSignature {
    access_stop_id: BoardingPointId,
    steps: Vec<StructuralStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum StructuralStep {
    Transit {
        service_id: TransitServiceId,
        board_stop_id: BoardingPointId,
        board_index: usize,
        alight_stop_id: BoardingPointId,
        alight_index: usize,
    },
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
    },
}

fn structural_steps(steps: &[StaticCorridorStep]) -> Vec<StructuralStep> {
    steps
        .iter()
        .map(|step| match step {
            StaticCorridorStep::Transit(leg) => StructuralStep::Transit {
                service_id: leg.service_id.clone(),
                board_stop_id: leg.board_stop_id.clone(),
                board_index: leg.board_index,
                alight_stop_id: leg.alight_stop_id.clone(),
                alight_index: leg.alight_index,
            },
            StaticCorridorStep::WalkingTransfer { from, to, .. } => {
                StructuralStep::WalkingTransfer {
                    from: from.clone(),
                    to: to.clone(),
                }
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateBoarding {
    pub service_id: TransitServiceId,
    pub board_index: usize,
    pub min_remaining_legs: usize,
}

/// The small union surface consumed by snapshot loading and temporal search.
#[derive(Clone, Debug, Default)]
pub struct CandidateCorridor {
    retained: Vec<StaticCorridor>,
    boardings_by_stop: HashMap<BoardingPointId, Vec<CandidateBoarding>>,
    minimums_by_position: HashMap<ServicePosition, usize>,
    selected_service_ids: HashSet<TransitServiceId>,
    selected_bus_stop_ids: HashSet<BoardingPointId>,
    terminal_transfers: HashSet<(BoardingPointId, BoardingPointId)>,
}

impl CandidateCorridor {
    fn from_retained(network: &TransitNetwork, retained: Vec<StaticCorridor>) -> Self {
        let mut minimums_by_position = HashMap::<ServicePosition, usize>::new();
        let mut selected_service_ids = HashSet::new();
        let mut selected_bus_stop_ids = HashSet::new();
        let mut terminal_transfers = HashSet::new();
        for corridor in &retained {
            if let Some(StaticCorridorStep::WalkingTransfer { from, to, .. }) =
                corridor.steps.last()
            {
                terminal_transfers.insert((from.clone(), to.clone()));
            }
            let legs = corridor.transit_legs().collect::<Vec<_>>();
            for (index, leg) in legs.iter().enumerate() {
                let remaining = legs.len() - index;
                let position = ServicePosition {
                    service_id: leg.service_id.clone(),
                    stop_index: leg.board_index,
                };
                minimums_by_position
                    .entry(position)
                    .and_modify(|known| *known = (*known).min(remaining))
                    .or_insert(remaining);
                selected_service_ids.insert(leg.service_id.clone());
                if network
                    .service(&leg.service_id)
                    .is_some_and(|service| service.transit_mode == TransitMode::Bus)
                {
                    selected_bus_stop_ids.insert(leg.board_stop_id.clone());
                }
            }
        }

        let mut boardings_by_stop = HashMap::<BoardingPointId, Vec<CandidateBoarding>>::new();
        for (position, min_remaining_legs) in &minimums_by_position {
            let Some(service) = network.service(&position.service_id) else {
                continue;
            };
            let Some(stop_id) = service.pattern.get(position.stop_index) else {
                continue;
            };
            boardings_by_stop
                .entry(stop_id.clone())
                .or_default()
                .push(CandidateBoarding {
                    service_id: position.service_id.clone(),
                    board_index: position.stop_index,
                    min_remaining_legs: *min_remaining_legs,
                });
        }
        for boardings in boardings_by_stop.values_mut() {
            boardings.sort_by(|left, right| {
                left.service_id
                    .as_str()
                    .cmp(right.service_id.as_str())
                    .then(left.board_index.cmp(&right.board_index))
            });
        }

        Self {
            retained,
            boardings_by_stop,
            minimums_by_position,
            selected_service_ids,
            selected_bus_stop_ids,
            terminal_transfers,
        }
    }

    pub fn retained(&self) -> &[StaticCorridor] {
        &self.retained
    }

    pub fn boardings_at(&self, stop: &BoardingPointId) -> &[CandidateBoarding] {
        self.boardings_by_stop
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn remaining_legs(
        &self,
        service_id: &TransitServiceId,
        board_index: usize,
    ) -> Option<usize> {
        self.minimums_by_position
            .get(&ServicePosition {
                service_id: service_id.clone(),
                stop_index: board_index,
            })
            .copied()
    }

    pub fn positions(&self) -> impl Iterator<Item = ServicePosition> + '_ {
        self.minimums_by_position.keys().cloned()
    }

    pub fn bus_stops(&self) -> Vec<BoardingPointId> {
        let mut stops = self
            .selected_bus_stop_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        stops.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        stops
    }

    pub fn len(&self) -> usize {
        self.minimums_by_position.len()
    }

    pub fn is_empty(&self) -> bool {
        self.retained.is_empty()
    }

    pub fn service_count(&self) -> usize {
        self.selected_service_ids.len()
    }

    pub fn allows_terminal_transfer(&self, from: &BoardingPointId, to: &BoardingPointId) -> bool {
        self.terminal_transfers
            .contains(&(from.clone(), to.clone()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StaticSearchStats {
    pub static_search_states_expanded: usize,
    pub static_service_scans: usize,
    pub static_positions_scanned: usize,
    pub static_complete_candidates_found: usize,
    pub static_unique_corridors_found: usize,
    pub static_corridors_retained: usize,
    pub static_corridor_services: usize,
    pub static_corridor_positions: usize,
    pub static_corridor_bus_stops: usize,
    pub static_search_latency_ms: u64,
    pub kth_static_duration_seconds: Option<u32>,
    pub exact_schedule_cost: usize,
    pub estimated_cost: usize,
    pub mixed_cost: usize,
    pub static_search_limit_reached: bool,
}

#[derive(Clone, Debug)]
pub struct StaticTravelTimeIndex {
    services: HashMap<TransitServiceId, ServiceTravelTimes>,
}

#[derive(Clone, Debug)]
struct ServiceTravelTimes {
    adjacent_seconds: Vec<u32>,
    source: StaticCostSource,
}

impl StaticTravelTimeIndex {
    pub fn new(network: &TransitNetwork) -> Self {
        let coordinates = network
            .stops
            .iter()
            .map(|stop| (stop.id.clone(), stop.coordinate))
            .collect::<HashMap<_, _>>();
        let mut scheduled = HashMap::<TransitServiceId, Vec<u32>>::new();
        for trip in &network.trips {
            let Some(service) = network.service(&trip.service_id) else {
                continue;
            };
            if trip.stop_times.len() != service.pattern.len() {
                continue;
            }
            let minimums = scheduled
                .entry(service.id.clone())
                .or_insert_with(|| vec![u32::MAX; service.pattern.len().saturating_sub(1)]);
            for (index, times) in trip.stop_times.windows(2).enumerate() {
                let duration = times[1]
                    .arrival_seconds
                    .saturating_sub(times[0].departure_seconds);
                if duration > 0 {
                    minimums[index] = minimums[index].min(duration);
                }
            }
        }

        let metro = network
            .metro_timings
            .iter()
            .map(|timing| {
                (
                    timing.service_id.clone(),
                    timing.inter_station_travel_seconds.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut services = HashMap::new();
        for service in &network.services {
            let exact = scheduled
                .remove(&service.id)
                .filter(|durations| durations.iter().all(|duration| *duration != u32::MAX))
                .or_else(|| {
                    metro.get(&service.id).and_then(|durations| {
                        (durations.len() == service.pattern.len().saturating_sub(1)
                            && durations.iter().all(|duration| *duration > 0))
                        .then(|| durations.clone())
                    })
                });
            let (adjacent_seconds, source) = match exact {
                Some(durations) => (durations, StaticCostSource::ExactSchedule),
                None => (
                    estimated_adjacent_seconds(&coordinates, service),
                    StaticCostSource::OptimisticEstimate,
                ),
            };
            services.insert(
                service.id.clone(),
                ServiceTravelTimes {
                    adjacent_seconds,
                    source,
                },
            );
        }
        Self { services }
    }

    fn ride(
        &self,
        service_id: &TransitServiceId,
        board_index: usize,
        alight_index: usize,
    ) -> Option<(u32, StaticCostSource)> {
        let service = self.services.get(service_id)?;
        let seconds = service
            .adjacent_seconds
            .get(board_index..alight_index)?
            .iter()
            .try_fold(0_u32, |total, duration| total.checked_add(*duration))?;
        (seconds > 0).then_some((seconds, service.source))
    }
}

fn estimated_adjacent_seconds(
    coordinates: &HashMap<BoardingPointId, RequestCoordinate>,
    service: &crate::domain::transit::TransitService,
) -> Vec<u32> {
    let metres_per_second = optimistic_kmh(service.transit_mode) / 3.6;
    service
        .pattern
        .windows(2)
        .map(|stops| {
            let distance = coordinates
                .get(&stops[0])
                .zip(coordinates.get(&stops[1]))
                .map(|(from, to)| from.haversine_distance_metres(*to))
                .unwrap_or(0.0);
            (distance / metres_per_second).ceil().max(1.0) as u32
        })
        .collect()
}

fn optimistic_kmh(mode: TransitMode) -> f64 {
    match mode {
        TransitMode::Bus => BUS_OPTIMISTIC_KMH,
        TransitMode::Metro => METRO_OPTIMISTIC_KMH,
        TransitMode::Rail => RAIL_OPTIMISTIC_KMH,
        TransitMode::HighSpeedRail => HIGH_SPEED_RAIL_OPTIMISTIC_KMH,
    }
}

#[derive(Clone, Debug)]
struct StaticState {
    stop_id: BoardingPointId,
    elapsed_seconds: u32,
    lower_bound_seconds: u32,
    priority_seconds: u32,
    access_stop_id: BoardingPointId,
    transit_legs_used: usize,
    steps: Vec<StaticCorridorStep>,
    has_exact_cost: bool,
    has_estimated_cost: bool,
    transfer_ready: bool,
}

impl StaticState {
    fn signature(&self) -> PartialSignature {
        PartialSignature {
            access_stop_id: self.access_stop_id.clone(),
            steps: structural_steps(&self.steps),
        }
    }

    fn cost_source(&self) -> StaticCostSource {
        match (self.has_exact_cost, self.has_estimated_cost) {
            (true, true) => StaticCostSource::Mixed,
            (true, false) => StaticCostSource::ExactSchedule,
            (false, true) | (false, false) => StaticCostSource::OptimisticEstimate,
        }
    }
}

#[derive(Clone, Debug)]
struct QueueEntry {
    state: StaticState,
    sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LowerBoundEntry {
    stop_id: BoardingPointId,
    seconds: u32,
    sequence: u64,
}

impl PartialOrd for LowerBoundEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LowerBoundEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .seconds
            .cmp(&self.seconds)
            .then(other.sequence.cmp(&self.sequence))
    }
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.state.priority_seconds == other.state.priority_seconds
            && self.state.lower_bound_seconds == other.state.lower_bound_seconds
            && self.state.elapsed_seconds == other.state.elapsed_seconds
            && self.sequence == other.sequence
    }
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .state
            .priority_seconds
            .cmp(&self.state.priority_seconds)
            .then(other.state.elapsed_seconds.cmp(&self.state.elapsed_seconds))
            .then(other.sequence.cmp(&self.sequence))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DominanceKey {
    stop_id: BoardingPointId,
    transit_legs_used: usize,
    transfer_ready: bool,
}

#[derive(Clone, Debug)]
struct KnownState {
    elapsed_seconds: u32,
    signature: PartialSignature,
}

pub fn generate(
    network: &TransitNetwork,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    travel_times: &StaticTravelTimeIndex,
    query: &TransitQuery,
) -> (CandidateCorridor, StaticSearchStats) {
    let started = Instant::now();
    let limit = query.max_static_corridors.max(1);
    let max_legs = query.max_transfers.saturating_add(1);
    let egress_by_stop = query.destination_egress.iter().fold(
        HashMap::<BoardingPointId, Vec<u32>>::new(),
        |mut by_stop, egress| {
            by_stop
                .entry(egress.stop_id.clone())
                .or_default()
                .push(egress.walking_seconds);
            by_stop
        },
    );
    let destination_lower_bounds =
        destination_lower_bounds(network, topology, transfers, travel_times, query);
    let mut frontier = BinaryHeap::new();
    let mut known = HashMap::<DominanceKey, Vec<KnownState>>::new();
    let mut sequence = 0_u64;
    for access in &query.origin_access {
        let Some(remaining_seconds) = destination_lower_bounds.get(&access.stop_id).copied() else {
            continue;
        };
        let elapsed_seconds = access.walking_seconds;
        let state = StaticState {
            stop_id: access.stop_id.clone(),
            elapsed_seconds,
            lower_bound_seconds: elapsed_seconds.saturating_add(remaining_seconds),
            priority_seconds: elapsed_seconds.saturating_add(remaining_seconds),
            access_stop_id: access.stop_id.clone(),
            transit_legs_used: 0,
            steps: Vec::new(),
            has_exact_cost: false,
            has_estimated_cost: false,
            transfer_ready: false,
        };
        enqueue_state(&mut frontier, &mut known, state, limit, &mut sequence);
    }

    let mut stats = StaticSearchStats::default();
    let mut retained = HashMap::<CorridorSignature, StaticCorridor>::new();
    let mut all_unique = HashSet::new();
    while let Some(entry) = frontier.pop() {
        if kth_duration(&retained, limit).is_some_and(|kth| {
            entry.state.lower_bound_seconds >= kth
                && frontier
                    .iter()
                    .all(|queued| queued.state.lower_bound_seconds >= kth)
        }) {
            break;
        }
        if !is_current(&known, &entry.state) {
            continue;
        }
        if stats.static_search_states_expanded >= query.max_static_search_expansions {
            stats.static_search_limit_reached = true;
            break;
        }
        stats.static_search_states_expanded += 1;
        let state = entry.state;

        if !state.transfer_ready {
            for shortcut in transfers.transfers_from(&state.stop_id) {
                let elapsed_seconds = state
                    .elapsed_seconds
                    .saturating_add(shortcut.duration_seconds);
                if elapsed_seconds > query.max_journey_duration_seconds {
                    continue;
                }
                let mut next = state.clone();
                next.stop_id = shortcut.to.clone();
                next.elapsed_seconds = elapsed_seconds;
                let Some(remaining_seconds) = destination_lower_bounds.get(&shortcut.to).copied()
                else {
                    continue;
                };
                next.lower_bound_seconds = elapsed_seconds.saturating_add(remaining_seconds);
                next.priority_seconds = next.lower_bound_seconds;
                next.steps.push(StaticCorridorStep::WalkingTransfer {
                    from: shortcut.from.clone(),
                    to: shortcut.to.clone(),
                    duration_seconds: shortcut.duration_seconds,
                });
                next.transfer_ready = true;
                if next.transit_legs_used > 0 {
                    record_complete_corridors(
                        &next,
                        &egress_by_stop,
                        query.max_journey_duration_seconds,
                        limit,
                        &mut stats,
                        &mut retained,
                        &mut all_unique,
                    );
                }
                if next.transit_legs_used < max_legs {
                    enqueue_state(&mut frontier, &mut known, next, limit, &mut sequence);
                }
            }
        }

        if state.transit_legs_used >= max_legs {
            continue;
        }

        for position in topology.occurrences_at(&state.stop_id) {
            let Some(service) = network.service(&position.service_id) else {
                continue;
            };
            if !query.allowed_transit_modes.contains(&service.transit_mode)
                || state.steps.iter().any(|step| {
                    matches!(
                        step,
                        StaticCorridorStep::Transit(leg)
                            if leg.service_id == position.service_id
                                && leg.board_index == position.stop_index
                    )
                })
            {
                continue;
            }
            stats.static_service_scans += 1;
            let transfer_seconds = if state.transit_legs_used > 0 && !state.transfer_ready {
                network.default_transfer_seconds
            } else {
                0
            };
            for alight_index in (position.stop_index + 1)..service.pattern.len() {
                stats.static_positions_scanned += 1;
                let Some((ride_seconds, source)) =
                    travel_times.ride(&service.id, position.stop_index, alight_index)
                else {
                    continue;
                };
                let elapsed_seconds = state
                    .elapsed_seconds
                    .saturating_add(transfer_seconds)
                    .saturating_add(ride_seconds);
                if elapsed_seconds > query.max_journey_duration_seconds {
                    continue;
                }
                let Some(alight_stop_id) = service.pattern.get(alight_index).cloned() else {
                    continue;
                };
                let mut next = state.clone();
                next.stop_id = alight_stop_id.clone();
                next.elapsed_seconds = elapsed_seconds;
                let Some(remaining_seconds) =
                    destination_lower_bounds.get(&alight_stop_id).copied()
                else {
                    continue;
                };
                next.lower_bound_seconds = elapsed_seconds.saturating_add(remaining_seconds);
                next.priority_seconds = next.lower_bound_seconds;
                next.transit_legs_used += 1;
                next.steps
                    .push(StaticCorridorStep::Transit(StaticTransitLeg {
                        service_id: service.id.clone(),
                        board_stop_id: state.stop_id.clone(),
                        board_index: position.stop_index,
                        alight_stop_id,
                        alight_index,
                        cost_source: source,
                    }));
                next.has_exact_cost |= source == StaticCostSource::ExactSchedule;
                next.has_estimated_cost |= source == StaticCostSource::OptimisticEstimate;
                next.transfer_ready = false;
                record_complete_corridors(
                    &next,
                    &egress_by_stop,
                    query.max_journey_duration_seconds,
                    limit,
                    &mut stats,
                    &mut retained,
                    &mut all_unique,
                );
                enqueue_state(&mut frontier, &mut known, next, limit, &mut sequence);
            }
        }
    }

    let mut retained = retained.into_values().collect::<Vec<_>>();
    retained.sort_by(corridor_order);
    retained.truncate(limit);
    let surface = CandidateCorridor::from_retained(network, retained);
    stats.static_corridors_retained = surface.retained().len();
    stats.static_corridor_services = surface.service_count();
    stats.static_corridor_positions = surface.len();
    stats.static_corridor_bus_stops = surface.bus_stops().len();
    stats.kth_static_duration_seconds = surface
        .retained()
        .get(limit.saturating_sub(1))
        .map(|corridor| corridor.theoretical_duration_seconds);
    for corridor in surface.retained() {
        match corridor.cost_source {
            StaticCostSource::ExactSchedule => stats.exact_schedule_cost += 1,
            StaticCostSource::OptimisticEstimate => stats.estimated_cost += 1,
            StaticCostSource::Mixed => stats.mixed_cost += 1,
        }
    }
    stats.static_search_latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    (surface, stats)
}

#[allow(clippy::too_many_arguments)]
fn record_complete_corridors(
    state: &StaticState,
    egress_by_stop: &HashMap<BoardingPointId, Vec<u32>>,
    max_journey_duration_seconds: u32,
    limit: usize,
    stats: &mut StaticSearchStats,
    retained: &mut HashMap<CorridorSignature, StaticCorridor>,
    all_unique: &mut HashSet<CorridorSignature>,
) {
    for egress_seconds in egress_by_stop.get(&state.stop_id).into_iter().flatten() {
        let duration = state.elapsed_seconds.saturating_add(*egress_seconds);
        if duration > max_journey_duration_seconds {
            continue;
        }
        stats.static_complete_candidates_found += 1;
        let corridor = StaticCorridor {
            access_stop_id: state.access_stop_id.clone(),
            egress_stop_id: state.stop_id.clone(),
            theoretical_duration_seconds: duration,
            cost_source: state.cost_source(),
            steps: state.steps.clone(),
        };
        let signature = corridor.signature();
        if all_unique.insert(signature.clone()) {
            stats.static_unique_corridors_found += 1;
        }
        let replace = retained.get(&signature).is_none_or(|known| {
            corridor.theoretical_duration_seconds < known.theoretical_duration_seconds
        });
        if replace {
            retained.insert(signature, corridor);
            trim_retained(retained, limit);
        }
    }
}

fn enqueue_state(
    frontier: &mut BinaryHeap<QueueEntry>,
    known: &mut HashMap<DominanceKey, Vec<KnownState>>,
    state: StaticState,
    limit: usize,
    sequence: &mut u64,
) {
    let key = DominanceKey {
        stop_id: state.stop_id.clone(),
        transit_legs_used: state.transit_legs_used,
        transfer_ready: state.transfer_ready,
    };
    let signature = state.signature();
    let alternatives = known.entry(key).or_default();
    if alternatives
        .iter()
        .any(|known| known.signature == signature && known.elapsed_seconds <= state.elapsed_seconds)
    {
        return;
    }
    if alternatives.len() >= limit
        && alternatives
            .iter()
            .filter(|known| known.elapsed_seconds <= state.elapsed_seconds)
            .count()
            >= limit
    {
        return;
    }
    alternatives.retain(|known| known.signature != signature);
    alternatives.push(KnownState {
        elapsed_seconds: state.elapsed_seconds,
        signature,
    });
    alternatives.sort_by_key(|known| known.elapsed_seconds);
    alternatives.truncate(limit);
    frontier.push(QueueEntry {
        state,
        sequence: *sequence,
    });
    *sequence = sequence.saturating_add(1);
}

fn is_current(known: &HashMap<DominanceKey, Vec<KnownState>>, state: &StaticState) -> bool {
    let key = DominanceKey {
        stop_id: state.stop_id.clone(),
        transit_legs_used: state.transit_legs_used,
        transfer_ready: state.transfer_ready,
    };
    let signature = state.signature();
    known.get(&key).is_some_and(|alternatives| {
        alternatives.iter().any(|known| {
            known.signature == signature && known.elapsed_seconds == state.elapsed_seconds
        })
    })
}

fn destination_lower_bounds(
    network: &TransitNetwork,
    topology: &TransitTopologyIndex,
    transfers: &TransferShortcutIndex,
    travel_times: &StaticTravelTimeIndex,
    query: &TransitQuery,
) -> HashMap<BoardingPointId, u32> {
    let mut lower_bounds = HashMap::<BoardingPointId, u32>::new();
    let mut frontier = BinaryHeap::new();
    let mut sequence = 0_u64;
    for egress in &query.destination_egress {
        if lower_bounds
            .get(&egress.stop_id)
            .is_some_and(|known| *known <= egress.walking_seconds)
        {
            continue;
        }
        lower_bounds.insert(egress.stop_id.clone(), egress.walking_seconds);
        frontier.push(LowerBoundEntry {
            stop_id: egress.stop_id.clone(),
            seconds: egress.walking_seconds,
            sequence,
        });
        sequence = sequence.saturating_add(1);
    }

    while let Some(entry) = frontier.pop() {
        if lower_bounds.get(&entry.stop_id) != Some(&entry.seconds) {
            continue;
        }
        for position in topology.occurrences_at(&entry.stop_id) {
            let Some(predecessor_index) = position.stop_index.checked_sub(1) else {
                continue;
            };
            let Some(service) = network.service(&position.service_id) else {
                continue;
            };
            if !query.allowed_transit_modes.contains(&service.transit_mode) {
                continue;
            }
            let Some(predecessor) = service.pattern.get(predecessor_index) else {
                continue;
            };
            let Some((edge_seconds, _)) =
                travel_times.ride(&service.id, predecessor_index, position.stop_index)
            else {
                continue;
            };
            relax_lower_bound(
                predecessor,
                entry.seconds.saturating_add(edge_seconds),
                &mut lower_bounds,
                &mut frontier,
                &mut sequence,
            );
        }
        for shortcut in transfers.transfers_to(&entry.stop_id) {
            relax_lower_bound(
                &shortcut.from,
                entry.seconds.saturating_add(shortcut.duration_seconds),
                &mut lower_bounds,
                &mut frontier,
                &mut sequence,
            );
        }
    }
    lower_bounds
}

fn relax_lower_bound(
    stop_id: &BoardingPointId,
    seconds: u32,
    lower_bounds: &mut HashMap<BoardingPointId, u32>,
    frontier: &mut BinaryHeap<LowerBoundEntry>,
    sequence: &mut u64,
) {
    if lower_bounds
        .get(stop_id)
        .is_some_and(|known| *known <= seconds)
    {
        return;
    }
    lower_bounds.insert(stop_id.clone(), seconds);
    frontier.push(LowerBoundEntry {
        stop_id: stop_id.clone(),
        seconds,
        sequence: *sequence,
    });
    *sequence = sequence.saturating_add(1);
}

fn kth_duration(
    retained: &HashMap<CorridorSignature, StaticCorridor>,
    limit: usize,
) -> Option<u32> {
    if retained.len() < limit {
        return None;
    }
    retained
        .values()
        .map(|corridor| corridor.theoretical_duration_seconds)
        .max()
}

fn trim_retained(retained: &mut HashMap<CorridorSignature, StaticCorridor>, limit: usize) {
    if retained.len() <= limit {
        return;
    }
    let worst = retained
        .iter()
        .max_by(|(_, left), (_, right)| corridor_order(left, right))
        .map(|(signature, _)| signature.clone());
    if let Some(worst) = worst {
        retained.remove(&worst);
    }
}

fn corridor_order(left: &StaticCorridor, right: &StaticCorridor) -> Ordering {
    left.theoretical_duration_seconds
        .cmp(&right.theoretical_duration_seconds)
        .then_with(|| format!("{:?}", left.signature()).cmp(&format!("{:?}", right.signature())))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use time::{Time, Weekday, macros::datetime};

    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::{
            ids::ScheduledTripId,
            transit::{
                ScheduledStopTime, ScheduledTrip, TransferEdge, TransitService, TransitStop,
            },
        },
        routing::{AccessPoint, EgressPoint},
    };

    fn id(value: &str) -> BoardingPointId {
        BoardingPointId::new(value)
    }

    fn service(value: &str, mode: TransitMode, pattern: &[&str]) -> TransitService {
        TransitService::new(
            TransitServiceId::new(value),
            mode,
            value,
            pattern.last().copied().unwrap_or_default(),
            pattern.iter().map(|stop| id(stop)).collect(),
        )
    }

    fn trip(value: &str, service_id: &str, offsets: &[u32]) -> ScheduledTrip {
        ScheduledTrip::weekdays(
            ScheduledTripId::new(value),
            TransitServiceId::new(service_id),
            vec![
                Weekday::Monday,
                Weekday::Tuesday,
                Weekday::Wednesday,
                Weekday::Thursday,
                Weekday::Friday,
                Weekday::Saturday,
                Weekday::Sunday,
            ],
            offsets
                .iter()
                .map(|seconds| ScheduledStopTime::new(*seconds, *seconds))
                .collect(),
        )
    }

    fn network(services: Vec<TransitService>, trips: Vec<ScheduledTrip>) -> TransitNetwork {
        let stop_ids = services
            .iter()
            .flat_map(|service| service.pattern.iter().cloned())
            .collect::<HashSet<_>>();
        TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            stop_ids
                .into_iter()
                .enumerate()
                .map(|(index, stop_id)| {
                    TransitStop::new(
                        stop_id.clone(),
                        stop_id.as_str(),
                        RequestCoordinate {
                            latitude: 25.0 + index as f64 * 0.00001,
                            longitude: 121.5,
                        },
                    )
                })
                .collect(),
            services,
            trips,
        )
    }

    fn query(
        origin: &str,
        destination: &str,
        modes: &[TransitMode],
        max_transfers: usize,
        k: usize,
    ) -> TransitQuery {
        TransitQuery::new(
            vec![AccessPoint::new(
                id(origin),
                datetime!(2026-09-21 10:00 UTC),
                datetime!(2026-09-21 10:00 UTC),
                0,
                0,
            )],
            vec![EgressPoint::new(id(destination), 0, 0)],
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes(modes.iter().copied())
        .with_limits(max_transfers, 16, 2_000, 3, 4 * 60 * 60)
        .with_max_static_corridors(k)
    }

    fn run(network: &TransitNetwork, query: &TransitQuery) -> CandidateCorridor {
        let topology = TransitTopologyIndex::new(network);
        let transfers = TransferShortcutIndex::from_network(network);
        let travel_times = StaticTravelTimeIndex::new(network);
        generate(network, &topology, &transfers, &travel_times, query).0
    }

    #[test]
    fn one_global_k_is_shared_across_bus_and_metro() {
        let network = network(
            vec![
                service("bus", TransitMode::Bus, &["o", "d"]),
                service("metro", TransitMode::Metro, &["o", "d"]),
            ],
            vec![
                trip("bus-trip", "bus", &[0, 100]),
                trip("metro-trip", "metro", &[0, 200]),
            ],
        );

        let surface = run(
            &network,
            &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 0, 1),
        );

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![TransitServiceId::new("bus")]
        );
    }

    #[test]
    fn mixed_bus_to_metro_can_outrank_slower_pure_bus() {
        let network = network(
            vec![
                service("connector", TransitMode::Bus, &["o", "hub"]),
                service("metro", TransitMode::Metro, &["hub", "d"]),
                service("slow-bus", TransitMode::Bus, &["o", "d"]),
            ],
            vec![
                trip("metro-trip", "metro", &[0, 300]),
                trip("slow-trip", "slow-bus", &[0, 1_200]),
            ],
        );

        let surface = run(
            &network,
            &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 1, 1),
        );

        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![
                TransitServiceId::new("connector"),
                TransitServiceId::new("metro")
            ]
        );
        assert_eq!(surface.retained()[0].cost_source, StaticCostSource::Mixed);
    }

    #[test]
    fn k_two_means_two_total_corridors_not_two_per_mode() {
        let network = network(
            vec![
                service("bus-a", TransitMode::Bus, &["o", "d"]),
                service("bus-b", TransitMode::Bus, &["o", "d"]),
                service("metro-a", TransitMode::Metro, &["o", "d"]),
                service("metro-b", TransitMode::Metro, &["o", "d"]),
            ],
            vec![
                trip("a", "bus-a", &[0, 100]),
                trip("b", "bus-b", &[0, 110]),
                trip("c", "metro-a", &[0, 120]),
                trip("d", "metro-b", &[0, 130]),
            ],
        );

        let surface = run(
            &network,
            &query("o", "d", &[TransitMode::Bus, TransitMode::Metro], 0, 2),
        );

        assert_eq!(surface.retained().len(), 2);
        assert_eq!(surface.service_count(), 2);
    }

    #[test]
    fn multiple_departures_of_one_structure_consume_one_slot() {
        let network = network(
            vec![service("bus", TransitMode::Bus, &["o", "d"])],
            vec![
                trip("morning", "bus", &[0, 100]),
                trip("evening", "bus", &[10_000, 10_100]),
            ],
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 100));

        assert_eq!(surface.retained().len(), 1);
    }

    #[test]
    fn much_slower_detour_is_excluded_when_k_is_full() {
        let network = network(
            vec![
                service("direct", TransitMode::Bus, &["o", "d"]),
                service("detour", TransitMode::Bus, &["o", "far", "d"]),
            ],
            vec![
                trip("direct-trip", "direct", &[0, 100]),
                trip("detour-trip", "detour", &[0, 1_800, 3_600]),
            ],
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 1));

        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![TransitServiceId::new("direct")]
        );
    }

    #[test]
    fn bus_without_exact_schedule_uses_optimistic_geometry() {
        let network = network(
            vec![service("realtime-only", TransitMode::Bus, &["o", "d"])],
            Vec::new(),
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 1));

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(
            surface.retained()[0].cost_source,
            StaticCostSource::OptimisticEstimate
        );
    }

    #[test]
    fn selected_mode_filtering_is_applied_before_retention() {
        let network = network(
            vec![
                service("bus", TransitMode::Bus, &["o", "d"]),
                service("metro", TransitMode::Metro, &["o", "d"]),
            ],
            vec![
                trip("bus-trip", "bus", &[0, 50]),
                trip("metro-trip", "metro", &[0, 100]),
            ],
        );

        let surface = run(&network, &query("o", "d", &[TransitMode::Metro], 0, 10));

        assert_eq!(surface.retained().len(), 1);
        assert_eq!(
            surface.retained()[0].service_sequence(),
            vec![TransitServiceId::new("metro")]
        );
    }

    #[test]
    fn maximum_transfer_count_is_respected() {
        let network = network(
            vec![
                service("first", TransitMode::Bus, &["o", "hub"]),
                service("second", TransitMode::Bus, &["hub", "d"]),
            ],
            vec![
                trip("first-trip", "first", &[0, 100]),
                trip("second-trip", "second", &[0, 100]),
            ],
        );

        let blocked = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 10));
        let allowed = run(&network, &query("o", "d", &[TransitMode::Bus], 1, 10));

        assert!(blocked.is_empty());
        assert_eq!(allowed.retained().len(), 1);
        assert_eq!(allowed.retained()[0].service_sequence().len(), 2);
    }

    #[test]
    fn static_walking_transfer_can_connect_access_to_first_boarding() {
        let network = network(
            vec![service("metro", TransitMode::Metro, &["board", "d"])],
            vec![trip("metro-trip", "metro", &[0, 100])],
        )
        .with_transfer_edges(vec![TransferEdge::new(id("access"), id("board"), 30)]);
        let mut network = network;
        network.stops.push(TransitStop::new(
            id("access"),
            "access",
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        ));

        let surface = run(
            &network,
            &query("access", "d", &[TransitMode::Metro], 0, 10),
        );

        assert_eq!(surface.retained().len(), 1);
        assert!(matches!(
            surface.retained()[0].steps.first(),
            Some(StaticCorridorStep::WalkingTransfer { .. })
        ));
    }

    #[test]
    fn static_walking_transfer_can_connect_final_alight_to_egress() {
        let network = network(
            vec![service("bus", TransitMode::Bus, &["o", "alight"])],
            vec![trip("bus-trip", "bus", &[0, 100])],
        )
        .with_transfer_edges(vec![TransferEdge::new(id("alight"), id("d"), 30)]);
        let mut network = network;
        network.stops.push(TransitStop::new(
            id("d"),
            "d",
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        ));

        let surface = run(&network, &query("o", "d", &[TransitMode::Bus], 0, 10));

        assert_eq!(surface.retained().len(), 1);
        assert!(matches!(
            surface.retained()[0].steps.last(),
            Some(StaticCorridorStep::WalkingTransfer { from, to, .. })
                if from == &id("alight") && to == &id("d")
        ));
    }

    #[test]
    fn expansion_bound_is_reported_instead_of_silently_certifying_partial_top_k() {
        let network = network(
            vec![
                service("first", TransitMode::Bus, &["o", "d"]),
                service("second", TransitMode::Bus, &["o", "d"]),
            ],
            vec![
                trip("first-trip", "first", &[0, 100]),
                trip("second-trip", "second", &[0, 110]),
            ],
        );
        let topology = TransitTopologyIndex::new(&network);
        let transfers = TransferShortcutIndex::from_network(&network);
        let travel_times = StaticTravelTimeIndex::new(&network);
        let mut query = query("o", "d", &[TransitMode::Bus], 0, 2);
        query.max_static_search_expansions = 1;

        let (_, stats) = generate(&network, &topology, &transfers, &travel_times, &query);

        assert!(stats.static_search_limit_reached);
    }
}
