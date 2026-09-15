//! Trip-Based transit routing.
//!
//! The index and queue state in this module are deliberately private to the
//! Trip-Based implementation. The public result is the algorithm-neutral
//! `JourneyCandidate`, so another algorithm does not need to adopt these
//! structures.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use time::{Duration, OffsetDateTime};

use crate::domain::{
    ids::{BoardingPointId, EffectiveTripId, TransitServiceId},
    realtime::BikeAvailability,
    transit::TransitNetwork,
    transit_timing::{
        DepartureError, DepartureQuery, DepartureResolver, EffectiveTrip, TimingBasis,
    },
};

use super::{
    AccessPoint, AlgorithmQuery, JourneyCandidate, JourneySegment, RoutingAlgorithm, RoutingError,
    transfers::{BikeAvailabilityResolver, TransferIndex, TransferShortcut, shortcut_is_usable},
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
    positions_by_stop: HashMap<BoardingPointId, Vec<LinePosition>>,
}

impl TripBasedIndex {
    pub fn new(network: &TransitNetwork) -> Self {
        let mut lines = Vec::<Line>::new();
        let mut service_to_line = HashMap::new();
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
        }
        for line in &mut lines {
            line.service_ids
                .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        let mut positions_by_stop = HashMap::<BoardingPointId, Vec<LinePosition>>::new();
        for (line_index, line) in lines.iter().enumerate() {
            for (stop_index, stop_id) in line.pattern.iter().enumerate() {
                positions_by_stop
                    .entry(stop_id.clone())
                    .or_default()
                    .push(LinePosition {
                        line_index,
                        stop_index,
                    });
            }
        }
        for positions in positions_by_stop.values_mut() {
            positions.sort_by(|left, right| {
                left.line_index
                    .cmp(&right.line_index)
                    .then(left.stop_index.cmp(&right.stop_index))
            });
        }
        Self {
            lines,
            service_to_line,
            positions_by_stop,
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

    pub fn services_at(&self, stop_id: &BoardingPointId) -> Vec<TransitServiceId> {
        let mut service_ids = self
            .positions_by_stop
            .get(stop_id)
            .into_iter()
            .flatten()
            .flat_map(|position| self.lines[position.line_index].service_ids.iter().cloned())
            .collect::<Vec<_>>();
        service_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        service_ids.dedup();
        service_ids
    }

    pub fn stop_index(
        &self,
        service_id: &TransitServiceId,
        stop_id: &BoardingPointId,
    ) -> Option<usize> {
        let line_index = *self.service_to_line.get(service_id)?;
        self.lines[line_index]
            .pattern
            .iter()
            .position(|candidate| candidate == stop_id)
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
    bike_availability: Option<Arc<dyn BikeAvailabilityResolver>>,
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
            bike_availability: None,
            limits: TripBasedLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: TripBasedLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_bike_availability(mut self, resolver: Arc<dyn BikeAvailabilityResolver>) -> Self {
        self.bike_availability = Some(resolver);
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
    bike_seconds: u32,
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
            bike_seconds: 0,
            transfer_count: 0,
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            uncertainty_seconds: None,
        }
    }

    fn with_transfer(&self, shortcut: &TransferShortcut) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::Transfer {
            shortcut: shortcut.clone(),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.walking_seconds);
        next.bike_seconds = next.bike_seconds.saturating_add(shortcut.bike_seconds);
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
    bike_seconds: u32,
}

#[derive(Clone, Copy)]
struct DynamicLabel {
    ready_at: OffsetDateTime,
    transfer_count: u32,
    walking_seconds: u32,
    bike_seconds: u32,
}

#[async_trait]
impl RoutingAlgorithm for TripBasedRouter {
    async fn route(&self, query: AlgorithmQuery) -> Result<Vec<JourneyCandidate>, RoutingError> {
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
        let mut availability_cache =
            HashMap::<crate::domain::ids::BikeShareStationId, BikeAvailability>::new();
        let initial_needs = self.initial_needs(&query);
        let initial_shortcuts = initial_needs
            .iter()
            .flat_map(|need| {
                need.path
                    .segments
                    .iter()
                    .filter_map(|segment| match segment {
                        JourneySegment::Transfer { shortcut } => Some(shortcut.clone()),
                        JourneySegment::Transit { .. } => None,
                    })
            })
            .collect::<Vec<_>>();
        let initial_availability = self
            .collect_shortcut_availability(&initial_shortcuts, &mut availability_cache)
            .await;
        let mut needs = initial_needs
            .into_iter()
            .filter(|need| {
                need.path.segments.iter().all(|segment| match segment {
                    JourneySegment::Transfer { shortcut } => {
                        shortcut_is_usable(shortcut, &initial_availability)
                    }
                    JourneySegment::Transit { .. } => true,
                })
            })
            .collect::<Vec<_>>();
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
                )
                .await;
            if states.is_empty() {
                break;
            }
            let availability = self
                .collect_availability(&states, &mut availability_cache)
                .await;
            let mut next_needs = Vec::new();
            for state in states {
                if expansions >= limits.max_expansions {
                    break;
                }
                expansions += 1;
                self.scan_state(
                    &query,
                    &limits,
                    &state,
                    &availability,
                    &mut candidates,
                    &mut next_needs,
                );
            }
            if expansions >= limits.max_expansions {
                break;
            }
            next_needs.sort_by(|left, right| {
                left.round
                    .cmp(&right.round)
                    .then(left.ready_at.cmp(&right.ready_at))
                    .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
            });
            needs = next_needs;
        }

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
                path: path.clone(),
            });
            for shortcut in self.transfer_index.transfers_from(&access.stop_id) {
                // Availability is checked before a shortcut is expanded. A
                // bike-less request still retains the walking alternative.
                needs.push(BoardingNeed {
                    stop_id: shortcut.to.clone(),
                    ready_at: access.available_at
                        + Duration::seconds(i64::from(shortcut.duration_seconds)),
                    round: 0,
                    previous_service: None,
                    path: path.with_transfer(shortcut),
                });
            }
        }
        needs
    }

    async fn resolve_boardings(
        &self,
        needs: Vec<BoardingNeed>,
        limits: &TripBasedLimits,
        cache: &mut HashMap<DepartureCacheKey, Result<Vec<EffectiveTrip>, DepartureError>>,
        reached_index: &mut HashMap<(EffectiveTripId, usize), Vec<ReachedTripLabel>>,
        dynamic_best: &mut HashMap<(TransitServiceId, usize, usize), Vec<DynamicLabel>>,
    ) -> Vec<TripState> {
        let mut unique_queries = Vec::<DepartureQuery>::new();
        let mut seen_queries = HashSet::<DepartureCacheKey>::new();
        for need in &needs {
            for service_id in self.index.services_at(&need.stop_id) {
                let query = DepartureQuery::new(
                    service_id,
                    need.stop_id.clone(),
                    need.ready_at,
                    limits.max_departures_per_query,
                );
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
        if !unique_queries.is_empty() {
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
                let board_index = self
                    .index
                    .stop_index(&service_id, &need.stop_id)
                    .unwrap_or_default();
                let dynamic_key = (service_id.clone(), board_index, need.round);
                if dynamic_best.get(&dynamic_key).is_some_and(|labels| {
                    labels.iter().any(|best| {
                        best.ready_at <= need.ready_at
                            && best.transfer_count <= need.path.transfer_count
                            && best.walking_seconds <= need.path.walking_seconds
                            && best.bike_seconds <= need.path.bike_seconds
                            && (best.ready_at < need.ready_at
                                || best.transfer_count < need.path.transfer_count
                                || best.walking_seconds < need.path.walking_seconds
                                || best.bike_seconds < need.path.bike_seconds)
                    })
                }) {
                    continue;
                }
                dynamic_best
                    .entry(dynamic_key)
                    .or_default()
                    .push(DynamicLabel {
                        ready_at: need.ready_at,
                        transfer_count: need.path.transfer_count,
                        walking_seconds: need.path.walking_seconds,
                        bike_seconds: need.path.bike_seconds,
                    });
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
                    if trip.service_id != service_id
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
                        bike_seconds: need.path.bike_seconds,
                    };
                    if reached_index.get(&reached_key).is_some_and(|labels| {
                        labels.iter().any(|reached| {
                            reached.board_index <= current_label.board_index
                                && reached.departure_at <= current_label.departure_at
                                && reached.transfer_count <= current_label.transfer_count
                                && reached.walking_seconds <= current_label.walking_seconds
                                && reached.bike_seconds <= current_label.bike_seconds
                                && (reached.board_index < current_label.board_index
                                    || reached.departure_at < current_label.departure_at
                                    || reached.transfer_count < current_label.transfer_count
                                    || reached.walking_seconds < current_label.walking_seconds
                                    || reached.bike_seconds < current_label.bike_seconds)
                        })
                    }) {
                        continue;
                    }
                    let labels = reached_index.entry(reached_key).or_default();
                    labels.retain(|reached| {
                        !(current_label.board_index <= reached.board_index
                            && current_label.departure_at <= reached.departure_at
                            && current_label.transfer_count <= reached.transfer_count
                            && current_label.walking_seconds <= reached.walking_seconds
                            && current_label.bike_seconds <= reached.bike_seconds
                            && (current_label.board_index < reached.board_index
                                || current_label.departure_at < reached.departure_at
                                || current_label.transfer_count < reached.transfer_count
                                || current_label.walking_seconds < reached.walking_seconds
                                || current_label.bike_seconds < reached.bike_seconds))
                    });
                    labels.push(current_label);
                    let changed = need
                        .previous_service
                        .as_ref()
                        .is_some_and(|previous| previous != &service_id);
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

    async fn collect_availability(
        &self,
        states: &[TripState],
        availability_cache: &mut HashMap<crate::domain::ids::BikeShareStationId, BikeAvailability>,
    ) -> HashMap<crate::domain::ids::BikeShareStationId, BikeAvailability> {
        let mut shortcuts = Vec::new();
        for state in states {
            for index in (state.board_index + 1)..state.trip.stop_times.len() {
                let stop_id = &state.trip.stop_times[index].stop_id;
                shortcuts.extend(self.transfer_index.transfers_from(stop_id).iter().cloned());
            }
        }
        self.collect_shortcut_availability(&shortcuts, availability_cache)
            .await
    }

    async fn collect_shortcut_availability(
        &self,
        shortcuts: &[TransferShortcut],
        availability_cache: &mut HashMap<crate::domain::ids::BikeShareStationId, BikeAvailability>,
    ) -> HashMap<crate::domain::ids::BikeShareStationId, BikeAvailability> {
        let Some(resolver) = &self.bike_availability else {
            return HashMap::new();
        };
        let mut station_ids = shortcuts
            .iter()
            .flat_map(TransferShortcut::required_station_ids)
            .collect::<Vec<_>>();
        station_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        station_ids.dedup();
        if station_ids.is_empty() {
            return HashMap::new();
        }
        let missing_station_ids = station_ids
            .iter()
            .filter(|station_id| !availability_cache.contains_key(*station_id))
            .cloned()
            .collect::<Vec<_>>();
        if !missing_station_ids.is_empty() {
            if let Ok(availability) = resolver.availability(&missing_station_ids).await {
                availability_cache.extend(availability);
            }
        }
        station_ids
            .into_iter()
            .filter_map(|station_id| {
                availability_cache
                    .get(&station_id)
                    .cloned()
                    .map(|availability| (station_id, availability))
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_state(
        &self,
        query: &AlgorithmQuery,
        limits: &TripBasedLimits,
        state: &TripState,
        availability: &HashMap<crate::domain::ids::BikeShareStationId, BikeAvailability>,
        candidates: &mut Vec<JourneyCandidate>,
        next_needs: &mut Vec<BoardingNeed>,
    ) {
        for alight_index in (state.board_index + 1)..state.trip.stop_times.len() {
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
            for egress in &query.destination_egress {
                if egress.stop_id != stop_time.stop_id {
                    continue;
                }
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
                candidates.push(JourneyCandidate {
                    access: path.access.clone(),
                    egress: egress.clone(),
                    segments: path.segments,
                    departure_time: path.access.departure_at,
                    arrival_time,
                    walking_seconds: path.walking_seconds,
                    bike_seconds: path.bike_seconds,
                    transfer_count: path.transfer_count,
                    uncertainty_seconds: path.uncertainty_seconds,
                    evidence: path.evidence.clone(),
                    timing_bases: path.timing_bases.clone(),
                });
            }

            if state.round >= limits.max_transfers {
                continue;
            }
            for shortcut in self.transfer_index.transfers_from(&stop_time.stop_id) {
                if shortcut.uses_bike_share() && !shortcut_is_usable(shortcut, availability) {
                    continue;
                }
                let mut path = state.path.clone();
                path.segments.push(transit_segment.clone());
                path = path.with_transfer(shortcut);
                next_needs.push(BoardingNeed {
                    stop_id: shortcut.to.clone(),
                    ready_at: arrival_at_stop
                        + Duration::seconds(i64::from(shortcut.duration_seconds)),
                    round: state.round + 1,
                    previous_service: Some(state.trip.service_id.clone()),
                    path,
                });
            }
        }
    }
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
