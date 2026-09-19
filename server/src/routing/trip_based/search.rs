use super::super::transfers::{TransferNeed, TransferTarget};
use super::super::{
    AlgorithmQuery, EgressPoint, JourneyCandidate, JourneySegment, RoutingAlgorithm, RoutingError,
};
use super::{
    TripBasedLimits, TripBasedRouter,
    boarding::BoardingRequest,
    diagnostics::SearchDiagnostics,
    dominance::{pareto_boarding_needs, pareto_candidates},
    state::{
        BoardingNeed, DepartureCacheKey, DynamicLabel, ReachedTripLabel, SearchPath, TripState,
    },
};
use crate::domain::ids::{EffectiveTripId, TransitServiceId};
use crate::domain::{
    ids::BoardingPointId,
    transit_timing::{DepartureError, EffectiveTrip},
};
use async_trait::async_trait;
use std::{collections::HashMap, sync::Arc, time::Instant};
use time::Duration;

const MAX_TRANSFER_BATCH: usize = 128;

/// A search path at a distinct-anchor movement opportunity. The ride segment
/// is appended only when the resolver returns a usable movement.
struct TransferSeed {
    stop_id: BoardingPointId,
    ready_at: time::OffsetDateTime,
    round: usize,
    previous_service: Option<TransitServiceId>,
    path: Arc<SearchPath>,
    ride: Option<(Arc<EffectiveTrip>, usize, usize)>,
}

struct ScanStateOutputs<'a> {
    egress_by_stop: &'a HashMap<BoardingPointId, Vec<EgressPoint>>,
    candidates: &'a mut Vec<JourneyCandidate>,
    next_needs: &'a mut Vec<BoardingNeed>,
    transfer_seeds: &'a mut Vec<TransferSeed>,
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
        let mut needs = self.initial_needs(&query).await;
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
                    BoardingRequest {
                        needs,
                        allowed_transit_modes: query.allowed_transit_modes.clone(),
                    },
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
            let mut transfer_seeds = Vec::new();
            for state in states {
                if expansions >= limits.max_expansions {
                    break;
                }
                expansions += 1;
                let mut outputs = ScanStateOutputs {
                    egress_by_stop: &egress_by_stop,
                    candidates: &mut candidates,
                    next_needs: &mut next_needs,
                    transfer_seeds: &mut transfer_seeds,
                    diagnostics: &mut diagnostics,
                };
                self.scan_state(&query, &limits, &state, &mut outputs);
            }
            if expansions >= limits.max_expansions {
                break;
            }
            next_needs.extend(
                self.expand_transfers(transfer_seeds, &mut diagnostics)
                    .await,
            );
            needs = pareto_boarding_needs(next_needs);
        }

        diagnostics.final_candidate_count = candidates.len();
        diagnostics.report(started_at);
        pareto_candidates(candidates, limits.max_candidates)
    }
}

impl TripBasedRouter {
    async fn initial_needs(&self, query: &AlgorithmQuery) -> Vec<BoardingNeed> {
        let mut needs = Vec::new();
        let mut transfer_seeds = Vec::new();
        for access in &query.origin_access {
            let path = SearchPath::from_access(access.clone());
            let need = BoardingNeed {
                stop_id: access.stop_id.clone(),
                ready_at: access.available_at,
                round: 0,
                previous_service: None,
                excluded_service: None,
                path: path.clone(),
            };
            transfer_seeds.push(TransferSeed {
                stop_id: access.stop_id.clone(),
                ready_at: access.available_at,
                round: 0,
                previous_service: None,
                path: Arc::new(path),
                ride: None,
            });
            needs.push(need);
        }
        needs.extend(
            self.expand_transfers(transfer_seeds, &mut SearchDiagnostics::default())
                .await,
        );
        pareto_boarding_needs(needs)
    }

    async fn expand_transfers(
        &self,
        seeds: Vec<TransferSeed>,
        diagnostics: &mut SearchDiagnostics,
    ) -> Vec<BoardingNeed> {
        let mut requests = Vec::new();
        let mut request_index = HashMap::<TransferNeed, usize>::new();
        let mut paths = Vec::<Vec<TransferSeed>>::new();
        for seed in seeds {
            let request = TransferNeed {
                source: seed.stop_id.clone(),
                ready_at: seed.ready_at,
                round: seed.round,
            };
            let index = *request_index.entry(request.clone()).or_insert_with(|| {
                let index = requests.len();
                requests.push(request);
                paths.push(Vec::new());
                index
            });
            paths[index].push(seed);
        }
        if requests.is_empty() {
            return Vec::new();
        }
        let mut resolved = Vec::new();
        for (batch_index, batch) in requests.chunks(MAX_TRANSFER_BATCH).enumerate() {
            let offset = batch_index * MAX_TRANSFER_BATCH;
            for mut transfer in self.transfer_resolver.expand_batch(batch).await {
                transfer.add_input_offset(offset);
                resolved.push(transfer);
            }
        }
        let mut needs = Vec::new();
        for transfer in resolved {
            let Some(request) = requests.get(transfer.input_index()) else {
                continue;
            };
            if transfer.need() != request {
                continue;
            }
            let Some(seeds) = paths.get(transfer.input_index()) else {
                continue;
            };
            let TransferTarget::BoardingPoint(target) = transfer.target();
            for seed in seeds {
                if transfer.source() != &seed.stop_id || target == &seed.stop_id {
                    continue;
                }
                let mut path = seed.path.as_ref().clone();
                if let Some((trip, board_index, alight_index)) = &seed.ride {
                    path.segments.push(JourneySegment::Transit {
                        trip: trip.as_ref().clone(),
                        board_index: *board_index,
                        alight_index: *alight_index,
                    });
                }
                needs.push(BoardingNeed {
                    stop_id: target.clone(),
                    ready_at: transfer.arrival_at(),
                    round: seed.round,
                    previous_service: seed.previous_service.clone(),
                    excluded_service: None,
                    path: path
                        .with_transfer(transfer.segment().clone(), transfer.walking_seconds()),
                });
                diagnostics.transfer_expansions += 1;
            }
        }
        needs
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
                trip: state.trip.as_ref().clone(),
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
                let mut path = state.path.as_ref().clone();
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
                    timing_reliability: path.timing_reliability,
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
                let mut path = state.path.as_ref().clone();
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
            outputs.transfer_seeds.push(TransferSeed {
                stop_id: stop_time.stop_id.clone(),
                ready_at: arrival_at_stop,
                round: state.round + 1,
                previous_service: Some(state.trip.service_id.clone()),
                path: state.path.clone(),
                ride: Some((state.trip.clone(), state.board_index, alight_index)),
            });
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
