//! Trip-Based transit routing.
//!
//! The index and queue state in this module are deliberately private to the
//! Trip-Based implementation. The public result is the algorithm-neutral
//! `JourneyCandidate`, so another algorithm does not need to adopt these
//! structures.

use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use async_trait::async_trait;
use time::{Duration, OffsetDateTime};

use crate::domain::{
    ids::{BoardingPointId, EffectiveTripId, TransitServiceId},
    transit::TransitNetwork,
    transit_timing::{
        DepartureError, DepartureQuery, DepartureResolver, EffectiveTrip, TimingBasis,
    },
};

use super::{
    AccessPoint, AlgorithmQuery, EgressPoint, JourneyCandidate, JourneySegment, RoutingAlgorithm,
    RoutingError,
    transfers::{TransferIndex, TransferShortcut},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub pattern: Vec<BoardingPointId>,
    pub service_ids: Vec<TransitServiceId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinePosition {
    pub line_index: usize,
    pub stop_index: usize,
}

/// Static Trip-Based index. It contains only service patterns and stable
/// schedule ordering; runtime trips are supplied by `DepartureResolver`.
#[derive(Clone, Debug, Default)]
pub struct TripBasedIndex {
    lines: Vec<Line>,
    service_to_line: HashMap<TransitServiceId, usize>,
    services_by_stop: HashMap<BoardingPointId, Vec<TransitServiceId>>,
    stop_indexes: HashMap<TransitServiceId, HashMap<BoardingPointId, usize>>,
}

impl TripBasedIndex {
    pub fn new(network: &TransitNetwork) -> Self {
        let mut lines = Vec::<Line>::new();
        let mut service_to_line = HashMap::new();
        let mut services_by_stop = HashMap::<BoardingPointId, Vec<TransitServiceId>>::new();
        let mut stop_indexes = HashMap::<TransitServiceId, HashMap<BoardingPointId, usize>>::new();
        for service in &network.services {
            let line_index = lines
                .iter()
                .position(|line| line.pattern == service.pattern)
                .unwrap_or_else(|| {
                    lines.push(Line {
                        pattern: service.pattern.clone(),
                        service_ids: Vec::new(),
                    });
                    lines.len() - 1
                });
            lines[line_index].service_ids.push(service.id.clone());
            service_to_line.insert(service.id.clone(), line_index);
            for (stop_index, stop_id) in service.pattern.iter().enumerate() {
                services_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(service.id.clone());
                stop_indexes
                    .entry(service.id.clone())
                    .or_default()
                    .entry(stop_id.clone())
                    .or_insert(stop_index);
            }
        }
        for line in &mut lines {
            line.service_ids
                .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        for service_ids in services_by_stop.values_mut() {
            service_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            service_ids.dedup();
        }
        Self {
            lines,
            service_to_line,
            services_by_stop,
            stop_indexes,
        }
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn line_for_service(&self, service_id: &TransitServiceId) -> Option<&Line> {
        self.service_to_line
            .get(service_id)
            .and_then(|index| self.lines.get(*index))
    }

    pub fn services_at(&self, stop_id: &BoardingPointId) -> &[TransitServiceId] {
        self.services_by_stop
            .get(stop_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn stop_index(
        &self,
        service_id: &TransitServiceId,
        stop_id: &BoardingPointId,
    ) -> Option<usize> {
        self.stop_indexes
            .get(service_id)
            .and_then(|stop_indexes| stop_indexes.get(stop_id))
            .copied()
    }

    pub fn pattern(&self, service_id: &TransitServiceId) -> Option<&[BoardingPointId]> {
        self.line_for_service(service_id)
            .map(|line| line.pattern.as_slice())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TripBasedLimits {
    pub max_transfers: usize,
    pub max_candidates: usize,
    pub max_expansions: usize,
    pub max_departures_per_query: usize,
    pub max_journey_duration_seconds: u32,
}

impl Default for TripBasedLimits {
    fn default() -> Self {
        Self {
            max_transfers: 4,
            max_candidates: 16,
            max_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
        }
    }
}

#[derive(Clone)]
pub struct TripBasedRouter {
    network: Arc<TransitNetwork>,
    index: Arc<TripBasedIndex>,
    departure_resolver: Arc<dyn DepartureResolver>,
    transfer_index: Arc<dyn TransferIndex>,
    limits: TripBasedLimits,
}

impl TripBasedRouter {
    pub fn new(
        network: Arc<TransitNetwork>,
        departure_resolver: Arc<dyn DepartureResolver>,
        transfer_index: Arc<dyn TransferIndex>,
    ) -> Self {
        let index = Arc::new(TripBasedIndex::new(&network));
        Self {
            network,
            index,
            departure_resolver,
            transfer_index,
            limits: TripBasedLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: TripBasedLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn index(&self) -> &TripBasedIndex {
        &self.index
    }

    pub fn network(&self) -> &Arc<TransitNetwork> {
        &self.network
    }

    pub fn limits(&self) -> &TripBasedLimits {
        &self.limits
    }
}

#[derive(Clone)]
struct SearchPath {
    access: AccessPoint,
    segments: Vec<JourneySegment>,
    walking_seconds: u32,
    transfer_count: u32,
    evidence: Vec<crate::domain::realtime::NormalizedRealtimeValue>,
    timing_bases: Vec<TimingBasis>,
    uncertainty_seconds: Option<u32>,
}

impl SearchPath {
    fn from_access(access: AccessPoint) -> Self {
        Self {
            walking_seconds: access.walking_seconds,
            access,
            segments: Vec::new(),
            transfer_count: 0,
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            uncertainty_seconds: None,
        }
    }

    fn with_transfer(&self, shortcut: &TransferShortcut) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: shortcut.clone(),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.duration_seconds);
        next
    }

    fn with_trip_timing(&self, trip: &EffectiveTrip, service_changed: bool) -> Self {
        let mut next = self.clone();
        if service_changed {
            next.transfer_count = next.transfer_count.saturating_add(1);
        }
        next.evidence.extend(trip.evidence.clone());
        next.timing_bases.push(trip.timing_basis);
        next.uncertainty_seconds = match (next.uncertainty_seconds, trip.uncertainty_seconds) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (None, value) | (value, None) => value,
        };
        next
    }
}

#[derive(Clone)]
struct BoardingNeed {
    stop_id: BoardingPointId,
    ready_at: OffsetDateTime,
    round: usize,
    previous_service: Option<TransitServiceId>,
    excluded_service: Option<TransitServiceId>,
    path: SearchPath,
}

#[derive(Clone)]
struct TripState {
    trip: EffectiveTrip,
    board_index: usize,
    round: usize,
    path: SearchPath,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DepartureCacheKey(DepartureQuery);

#[derive(Clone, Copy)]
struct ReachedTripLabel {
    board_index: usize,
    departure_at: OffsetDateTime,
    transfer_count: u32,
    walking_seconds: u32,
}

#[derive(Clone, Copy)]
struct DynamicLabel {
    ready_at: OffsetDateTime,
    transfer_count: u32,
    walking_seconds: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct SearchDiagnostics {
    access_candidate_count: usize,
    egress_candidate_count: usize,
    initial_need_count: usize,
    unique_departure_query_count: usize,
    departure_batch_count: usize,
    trip_state_count: usize,
    scanned_downstream_stop_count: usize,
    walking_transfer_expansions: usize,
    same_stop_transfer_expansions: usize,
    dynamic_labels_accepted: usize,
    dynamic_labels_pruned: usize,
    reached_labels_accepted: usize,
    reached_labels_pruned: usize,
    final_candidate_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BoardingNeedKey {
    stop_id: BoardingPointId,
    round: usize,
    previous_service: Option<TransitServiceId>,
    excluded_service: Option<TransitServiceId>,
}

struct ScanStateOutputs<'a> {
    egress_by_stop: &'a HashMap<BoardingPointId, Vec<EgressPoint>>,
    candidates: &'a mut Vec<JourneyCandidate>,
    next_needs: &'a mut Vec<BoardingNeed>,
    diagnostics: &'a mut SearchDiagnostics,
}

#[async_trait]
impl RoutingAlgorithm for TripBasedRouter {
    async fn route(&self, query: AlgorithmQuery) -> Result<Vec<JourneyCandidate>, RoutingError> {
        let started_at = Instant::now();
        if query.origin_access.is_empty() || query.destination_egress.is_empty() {
            return Err(RoutingError::InvalidQuery);
        }
        if query.max_expansions == 0 {
            return Ok(Vec::new());
        }
        let limits = TripBasedLimits {
            max_transfers: query.max_transfers.min(self.limits.max_transfers),
            max_candidates: query.max_candidates.min(self.limits.max_candidates).max(1),
            max_expansions: query.max_expansions.min(self.limits.max_expansions),
            max_departures_per_query: self.limits.max_departures_per_query.max(1),
            max_journey_duration_seconds: query
                .max_journey_duration_seconds
                .min(self.limits.max_journey_duration_seconds),
        };

        let mut departure_cache =
            HashMap::<DepartureCacheKey, Result<Vec<EffectiveTrip>, DepartureError>>::new();
        let mut reached_index = HashMap::<(EffectiveTripId, usize), Vec<ReachedTripLabel>>::new();
        let mut dynamic_best =
            HashMap::<(TransitServiceId, usize, usize), Vec<DynamicLabel>>::new();
        let egress_by_stop = index_egress_points(&query.destination_egress);
        let mut needs = self.initial_needs(&query);
        let mut diagnostics = SearchDiagnostics {
            access_candidate_count: query.origin_access.len(),
            egress_candidate_count: query.destination_egress.len(),
            initial_need_count: needs.len(),
            ..SearchDiagnostics::default()
        };
        let mut candidates = Vec::new();
        let mut expansions = 0usize;

        for round in 0..=limits.max_transfers {
            if needs.is_empty() {
                break;
            }
            needs.retain(|need| need.round == round);
            if needs.is_empty() {
                continue;
            }
            let states = self
                .resolve_boardings(
                    needs,
                    &limits,
                    &mut departure_cache,
                    &mut reached_index,
                    &mut dynamic_best,
                    &mut diagnostics,
                )
                .await;
            if states.is_empty() {
                needs = Vec::new();
                continue;
            }
            diagnostics.trip_state_count += states.len();
            let mut next_needs = Vec::new();
            for state in states {
                if expansions >= limits.max_expansions {
                    break;
                }
                expansions += 1;
                let mut outputs = ScanStateOutputs {
                    egress_by_stop: &egress_by_stop,
                    candidates: &mut candidates,
                    next_needs: &mut next_needs,
                    diagnostics: &mut diagnostics,
                };
                self.scan_state(&query, &limits, &state, &mut outputs);
            }
            if expansions >= limits.max_expansions {
                break;
            }
            needs = pareto_boarding_needs(next_needs);
        }

        diagnostics.final_candidate_count = candidates.len();
        tracing::debug!(
            target: "via::routing",
            access_candidates = diagnostics.access_candidate_count,
            egress_candidates = diagnostics.egress_candidate_count,
            initial_needs = diagnostics.initial_need_count,
            unique_departure_queries = diagnostics.unique_departure_query_count,
            departure_batches = diagnostics.departure_batch_count,
            trip_states = diagnostics.trip_state_count,
            scanned_downstream_stops = diagnostics.scanned_downstream_stop_count,
            walking_transfer_expansions = diagnostics.walking_transfer_expansions,
            same_stop_transfer_expansions = diagnostics.same_stop_transfer_expansions,
            dynamic_labels_accepted = diagnostics.dynamic_labels_accepted,
            dynamic_labels_pruned = diagnostics.dynamic_labels_pruned,
            reached_labels_accepted = diagnostics.reached_labels_accepted,
            reached_labels_pruned = diagnostics.reached_labels_pruned,
            final_candidates = diagnostics.final_candidate_count,
            search_latency_ms = started_at.elapsed().as_millis() as u64,
            "trip-based route search diagnostics"
        );
        pareto_candidates(candidates, limits.max_candidates)
    }
}

impl TripBasedRouter {
    fn initial_needs(&self, query: &AlgorithmQuery) -> Vec<BoardingNeed> {
        let mut needs = Vec::new();
        for access in &query.origin_access {
            let path = SearchPath::from_access(access.clone());
            needs.push(BoardingNeed {
                stop_id: access.stop_id.clone(),
                ready_at: access.available_at,
                round: 0,
                previous_service: None,
                excluded_service: None,
                path: path.clone(),
            });
            for shortcut in self.transfer_index.transfers_from(&access.stop_id) {
                needs.push(BoardingNeed {
                    stop_id: shortcut.to.clone(),
                    ready_at: access.available_at
                        + Duration::seconds(i64::from(shortcut.duration_seconds)),
                    round: 0,
                    previous_service: None,
                    excluded_service: None,
                    path: path.with_transfer(shortcut),
                });
            }
        }
        pareto_boarding_needs(needs)
    }

    async fn resolve_boardings(
        &self,
        needs: Vec<BoardingNeed>,
        limits: &TripBasedLimits,
        cache: &mut HashMap<DepartureCacheKey, Result<Vec<EffectiveTrip>, DepartureError>>,
        reached_index: &mut HashMap<(EffectiveTripId, usize), Vec<ReachedTripLabel>>,
        dynamic_best: &mut HashMap<(TransitServiceId, usize, usize), Vec<DynamicLabel>>,
        diagnostics: &mut SearchDiagnostics,
    ) -> Vec<TripState> {
        let mut query_frontier =
            HashMap::<(TransitServiceId, usize, usize), Vec<(DynamicLabel, DepartureQuery)>>::new();
        for need in &needs {
            for service_id in self.index.services_at(&need.stop_id) {
                if need
                    .excluded_service
                    .as_ref()
                    .is_some_and(|excluded| excluded == service_id)
                {
                    continue;
                }
                let board_index = self
                    .index
                    .stop_index(service_id, &need.stop_id)
                    .unwrap_or_default();
                let dynamic_key = (service_id.clone(), board_index, need.round);
                let label = DynamicLabel {
                    ready_at: need.ready_at,
                    transfer_count: need.path.transfer_count,
                    walking_seconds: need.path.walking_seconds,
                };
                if dynamic_best.get(&dynamic_key).is_some_and(|labels| {
                    labels
                        .iter()
                        .any(|existing| dynamic_label_dominates_or_equals(existing, &label))
                }) {
                    continue;
                }
                let query = DepartureQuery::new(
                    service_id.clone(),
                    need.stop_id.clone(),
                    need.ready_at,
                    limits.max_departures_per_query,
                );
                let labels = query_frontier.entry(dynamic_key).or_default();
                if labels
                    .iter()
                    .any(|(existing, _)| dynamic_label_dominates_or_equals(existing, &label))
                {
                    continue;
                }
                labels.retain(|(existing, _)| !dynamic_label_dominates(&label, existing));
                labels.push((label, query));
            }
        }
        let mut unique_queries = Vec::<DepartureQuery>::new();
        let mut seen_queries = HashSet::<DepartureCacheKey>::new();
        for candidates in query_frontier.into_values() {
            for (_, query) in candidates {
                let key = DepartureCacheKey(query.clone());
                if !cache.contains_key(&key) && seen_queries.insert(key) {
                    unique_queries.push(query);
                }
            }
        }
        unique_queries.sort_by(|left, right| {
            left.service_id
                .as_str()
                .cmp(right.service_id.as_str())
                .then(
                    left.board_stop_id
                        .as_str()
                        .cmp(right.board_stop_id.as_str()),
                )
                .then(left.after.cmp(&right.after))
        });
        diagnostics.unique_departure_query_count += unique_queries.len();
        if !unique_queries.is_empty() {
            diagnostics.departure_batch_count += 1;
            let results = self
                .departure_resolver
                .next_departures_batch(unique_queries.clone())
                .await;
            for (index, query) in unique_queries.into_iter().enumerate() {
                let result = results
                    .get(index)
                    .cloned()
                    .unwrap_or(Err(DepartureError::TemporarilyUnavailable));
                cache.insert(DepartureCacheKey(query), result);
            }
        }

        let mut states = Vec::new();
        for need in needs {
            for service_id in self.index.services_at(&need.stop_id) {
                if need
                    .excluded_service
                    .as_ref()
                    .is_some_and(|excluded| excluded == service_id)
                {
                    continue;
                }
                let board_index = self
                    .index
                    .stop_index(service_id, &need.stop_id)
                    .unwrap_or_default();
                let dynamic_key = (service_id.clone(), board_index, need.round);
                let label = DynamicLabel {
                    ready_at: need.ready_at,
                    transfer_count: need.path.transfer_count,
                    walking_seconds: need.path.walking_seconds,
                };
                let labels = dynamic_best.entry(dynamic_key).or_default();
                let Some(removed) = insert_dynamic_label(labels, label) else {
                    diagnostics.dynamic_labels_pruned += 1;
                    continue;
                };
                diagnostics.dynamic_labels_accepted += 1;
                diagnostics.dynamic_labels_pruned += removed;
                let query = DepartureQuery::new(
                    service_id.clone(),
                    need.stop_id.clone(),
                    need.ready_at,
                    limits.max_departures_per_query,
                );
                let Some(Ok(trips)) = cache.get(&DepartureCacheKey(query)) else {
                    continue;
                };
                for trip in trips {
                    if &trip.service_id != service_id
                        || trip.stop_times.len() <= board_index + 1
                        || trip.stop_times.get(board_index).is_none_or(|stop_time| {
                            stop_time.stop_id != need.stop_id
                                || stop_time.departure_at < need.ready_at
                        })
                    {
                        continue;
                    }
                    let departure_at = trip.stop_times[board_index].departure_at;
                    let reached_key = (trip.id.clone(), need.round);
                    let current_label = ReachedTripLabel {
                        board_index,
                        departure_at,
                        transfer_count: need.path.transfer_count,
                        walking_seconds: need.path.walking_seconds,
                    };
                    if reached_index.get(&reached_key).is_some_and(|labels| {
                        labels.iter().any(|reached| {
                            reached.board_index <= current_label.board_index
                                && reached.departure_at <= current_label.departure_at
                                && reached.transfer_count <= current_label.transfer_count
                                && reached.walking_seconds <= current_label.walking_seconds
                                && (reached.board_index < current_label.board_index
                                    || reached.departure_at < current_label.departure_at
                                    || reached.transfer_count < current_label.transfer_count
                                    || reached.walking_seconds < current_label.walking_seconds)
                        })
                    }) {
                        diagnostics.reached_labels_pruned += 1;
                        continue;
                    }
                    let labels = reached_index.entry(reached_key).or_default();
                    let label_count = labels.len();
                    labels.retain(|reached| {
                        !(current_label.board_index <= reached.board_index
                            && current_label.departure_at <= reached.departure_at
                            && current_label.transfer_count <= reached.transfer_count
                            && current_label.walking_seconds <= reached.walking_seconds
                            && (current_label.board_index < reached.board_index
                                || current_label.departure_at < reached.departure_at
                                || current_label.transfer_count < reached.transfer_count
                                || current_label.walking_seconds < reached.walking_seconds))
                    });
                    diagnostics.reached_labels_pruned += label_count.saturating_sub(labels.len());
                    labels.push(current_label);
                    diagnostics.reached_labels_accepted += 1;
                    let changed = need
                        .previous_service
                        .as_ref()
                        .is_some_and(|previous| previous != service_id);
                    let path = need.path.with_trip_timing(trip, changed);
                    states.push(TripState {
                        trip: trip.clone(),
                        board_index,
                        round: need.round,
                        path,
                    });
                }
            }
        }
        states.sort_by(|left, right| {
            left.trip.stop_times[left.board_index]
                .departure_at
                .cmp(&right.trip.stop_times[right.board_index].departure_at)
                .then(left.trip.id.as_str().cmp(right.trip.id.as_str()))
        });
        states
    }

    fn scan_state(
        &self,
        query: &AlgorithmQuery,
        limits: &TripBasedLimits,
        state: &TripState,
        outputs: &mut ScanStateOutputs<'_>,
    ) {
        for alight_index in (state.board_index + 1)..state.trip.stop_times.len() {
            outputs.diagnostics.scanned_downstream_stop_count += 1;
            let stop_time = &state.trip.stop_times[alight_index];
            let arrival_at_stop = stop_time.arrival_at;
            let elapsed = arrival_at_stop - query.departure_time;
            if elapsed > Duration::seconds(i64::from(limits.max_journey_duration_seconds)) {
                continue;
            }
            let transit_segment = JourneySegment::Transit {
                trip: state.trip.clone(),
                board_index: state.board_index,
                alight_index,
            };
            for egress in outputs
                .egress_by_stop
                .get(&stop_time.stop_id)
                .into_iter()
                .flatten()
            {
                let arrival_time =
                    arrival_at_stop + Duration::seconds(i64::from(egress.walking_seconds));
                if query
                    .latest_arrival
                    .is_some_and(|latest| arrival_time > latest)
                    || arrival_time - query.departure_time
                        > Duration::seconds(i64::from(limits.max_journey_duration_seconds))
                {
                    continue;
                }
                let mut path = state.path.clone();
                path.segments.push(transit_segment.clone());
                path.walking_seconds = path.walking_seconds.saturating_add(egress.walking_seconds);
                outputs.candidates.push(JourneyCandidate {
                    access: path.access.clone(),
                    egress: egress.clone(),
                    segments: path.segments,
                    departure_time: path.access.departure_at,
                    arrival_time,
                    walking_seconds: path.walking_seconds,
                    transfer_count: path.transfer_count,
                    uncertainty_seconds: path.uncertainty_seconds,
                    evidence: path.evidence.clone(),
                    timing_bases: path.timing_bases.clone(),
                });
            }

            if state.round >= limits.max_transfers {
                continue;
            }
            let has_same_stop_alternative =
                self.index
                    .services_at(&stop_time.stop_id)
                    .iter()
                    .any(|service_id| {
                        service_id != &state.trip.service_id
                            && self
                                .index
                                .stop_index(service_id, &stop_time.stop_id)
                                .and_then(|board_index| {
                                    self.index
                                        .pattern(service_id)
                                        .map(|pattern| board_index + 1 < pattern.len())
                                })
                                .unwrap_or(false)
                    });
            if has_same_stop_alternative {
                let mut path = state.path.clone();
                path.segments.push(transit_segment.clone());
                outputs.next_needs.push(BoardingNeed {
                    stop_id: stop_time.stop_id.clone(),
                    ready_at: arrival_at_stop
                        + Duration::seconds(i64::from(self.network.default_transfer_seconds)),
                    round: state.round + 1,
                    previous_service: Some(state.trip.service_id.clone()),
                    excluded_service: Some(state.trip.service_id.clone()),
                    path,
                });
                outputs.diagnostics.same_stop_transfer_expansions += 1;
            }
            for shortcut in self.transfer_index.transfers_from(&stop_time.stop_id) {
                let mut path = state.path.clone();
                path.segments.push(transit_segment.clone());
                path = path.with_transfer(shortcut);
                outputs.next_needs.push(BoardingNeed {
                    stop_id: shortcut.to.clone(),
                    ready_at: arrival_at_stop
                        + Duration::seconds(i64::from(shortcut.duration_seconds)),
                    round: state.round + 1,
                    previous_service: Some(state.trip.service_id.clone()),
                    excluded_service: None,
                    path,
                });
                outputs.diagnostics.walking_transfer_expansions += 1;
            }
        }
    }
}

fn index_egress_points(
    egress_points: &[EgressPoint],
) -> HashMap<BoardingPointId, Vec<EgressPoint>> {
    let mut by_stop = HashMap::<BoardingPointId, Vec<EgressPoint>>::new();
    for egress in egress_points {
        by_stop
            .entry(egress.stop_id.clone())
            .or_default()
            .push(egress.clone());
    }
    by_stop
}

fn pareto_boarding_needs(needs: Vec<BoardingNeed>) -> Vec<BoardingNeed> {
    let mut groups = HashMap::<BoardingNeedKey, Vec<BoardingNeed>>::new();
    for need in needs {
        let key = BoardingNeedKey {
            stop_id: need.stop_id.clone(),
            round: need.round,
            previous_service: need.previous_service.clone(),
            excluded_service: need.excluded_service.clone(),
        };
        groups.entry(key).or_default().push(need);
    }
    let mut pareto = groups
        .into_values()
        .flat_map(pareto_boarding_need_group)
        .collect::<Vec<_>>();
    pareto.sort_by(|left, right| {
        left.stop_id
            .as_str()
            .cmp(right.stop_id.as_str())
            .then(left.round.cmp(&right.round))
            .then(left.ready_at.cmp(&right.ready_at))
            .then(left.path.walking_seconds.cmp(&right.path.walking_seconds))
            .then(left.path.transfer_count.cmp(&right.path.transfer_count))
    });
    pareto
}

fn pareto_boarding_need_group(mut needs: Vec<BoardingNeed>) -> Vec<BoardingNeed> {
    needs.sort_by(|left, right| {
        left.ready_at
            .cmp(&right.ready_at)
            .then(left.path.walking_seconds.cmp(&right.path.walking_seconds))
            .then(left.path.transfer_count.cmp(&right.path.transfer_count))
    });
    let mut pareto = Vec::with_capacity(needs.len());
    for need in needs {
        if pareto
            .iter()
            .any(|existing| boarding_need_dominates(existing, &need))
        {
            continue;
        }
        pareto.retain(|existing| !boarding_need_dominates(&need, existing));
        pareto.push(need);
    }
    pareto
}

fn boarding_need_dominates(left: &BoardingNeed, right: &BoardingNeed) -> bool {
    left.stop_id == right.stop_id
        && left.round == right.round
        && left.previous_service == right.previous_service
        && left.excluded_service == right.excluded_service
        && left.ready_at <= right.ready_at
        && left.path.walking_seconds <= right.path.walking_seconds
        && left.path.transfer_count <= right.path.transfer_count
        && (left.ready_at < right.ready_at
            || left.path.walking_seconds < right.path.walking_seconds
            || left.path.transfer_count < right.path.transfer_count)
}

fn insert_dynamic_label(labels: &mut Vec<DynamicLabel>, label: DynamicLabel) -> Option<usize> {
    if labels
        .iter()
        .any(|existing| dynamic_label_dominates_or_equals(existing, &label))
    {
        return None;
    }
    let old_len = labels.len();
    labels.retain(|existing| !dynamic_label_dominates(&label, existing));
    labels.push(label);
    Some(old_len.saturating_sub(labels.len().saturating_sub(1)))
}

fn dynamic_label_dominates_or_equals(left: &DynamicLabel, right: &DynamicLabel) -> bool {
    left.ready_at <= right.ready_at
        && left.transfer_count <= right.transfer_count
        && left.walking_seconds <= right.walking_seconds
}

fn dynamic_label_dominates(left: &DynamicLabel, right: &DynamicLabel) -> bool {
    dynamic_label_dominates_or_equals(left, right)
        && (left.ready_at < right.ready_at
            || left.transfer_count < right.transfer_count
            || left.walking_seconds < right.walking_seconds)
}

fn pareto_candidates(
    mut candidates: Vec<JourneyCandidate>,
    limit: usize,
) -> Result<Vec<JourneyCandidate>, RoutingError> {
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
    pareto.sort_by(|left, right| {
        left.arrival_time
            .cmp(&right.arrival_time)
            .then(left.transfer_count.cmp(&right.transfer_count))
            .then(left.signature().cmp(&right.signature()))
    });
    pareto.truncate(limit.max(1));
    Ok(pareto)
}

#[cfg(test)]
mod tests {
    use time::{Duration, macros::datetime};

    use super::{DynamicLabel, insert_dynamic_label};

    fn label(offset: i64, transfers: u32, walking: u32) -> DynamicLabel {
        DynamicLabel {
            ready_at: datetime!(2026-08-31 10:00 UTC) + Duration::seconds(offset),
            transfer_count: transfers,
            walking_seconds: walking,
        }
    }

    #[test]
    fn dynamic_frontier_rejects_a_dominated_label_in_any_arrival_order() {
        let good = label(0, 0, 0);
        let bad = label(60, 1, 30);
        let mut good_first = vec![good];
        assert!(insert_dynamic_label(&mut good_first, bad).is_none());

        let mut bad_first = vec![bad];
        assert_eq!(insert_dynamic_label(&mut bad_first, good), Some(1));
        assert_eq!(bad_first.len(), 1);
        assert_eq!(bad_first[0].ready_at, good.ready_at);
        assert_eq!(bad_first[0].transfer_count, good.transfer_count);
        assert_eq!(bad_first[0].walking_seconds, good.walking_seconds);
    }

    #[test]
    fn dynamic_frontier_deduplicates_equal_labels() {
        let same = label(60, 1, 30);
        let mut labels = vec![same];
        assert!(insert_dynamic_label(&mut labels, same).is_none());
        assert_eq!(labels.len(), 1);
    }
}
