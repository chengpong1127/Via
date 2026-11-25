pub(crate) mod bike;
mod pareto;
mod state;
#[cfg(test)]
mod tests;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use futures_util::{StreamExt, TryStreamExt, stream};
use time::{Duration, OffsetDateTime};
use tracing::Instrument;

use super::{
    cache::MatrixCache,
    corridors::{Corridor, CorridorAnchor, PlannedMovement, StaticTransitLeg},
    journey::{
        BIKE_REUSE_COOLDOWN, BikeShareMovement, JourneyAnchor, JourneyAnchorKind, JourneyMovement,
        JourneyMovementKind, RealizedJourney, WalkingMovementKind,
    },
    model::{FailureSummary, PlanningQuery, RoutingError},
};
use crate::{
    domain::{
        ids::BoardingPointId,
        realtime::NormalizedRealtimeValue,
        transit::runtime::TransitRuntime,
        transit::timing::{EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot},
    },
    street::{
        Coordinate, MatrixCell, RouteQuery, RoutingConstraints, StreetRoutingError,
        StreetTravelMode,
    },
};
use bike::BikeMovementSource;
use pareto::{EvaluatedCandidate, pareto_candidates, prune_states};
use state::{EvaluationState, PathMetrics};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CorridorEvaluationStats {
    pub corridors_evaluated: usize,
    pub states_expanded: usize,
    pub successors_generated: usize,
    pub transit_departure_lookups: usize,
    pub final_candidates: usize,
    pub street_requests: usize,
    pub street_cache_hits: usize,
    pub bike_requests: usize,
    pub bike_cache_hits: usize,
    pub latency_us: u64,
}

pub(super) struct Evaluation {
    pub journeys: Vec<RealizedJourney>,
    pub stats: CorridorEvaluationStats,
    pub failures: FailureSummary,
}

pub(super) struct CorridorEvaluator<'a> {
    pub transit: &'a TransitRuntime,
    pub timing: &'a TimingSnapshot,
    pub query: &'a PlanningQuery,
    pub matrix_cache: &'a MatrixCache,
    pub bike_movements: Option<&'a dyn BikeMovementSource>,
}

#[derive(Default)]
struct EvaluationCounters {
    corridors: AtomicUsize,
    states: AtomicUsize,
    successors: AtomicUsize,
    departure_lookups: AtomicUsize,
}

struct CorridorEvaluationRun<'e, 'a> {
    evaluator: &'e CorridorEvaluator<'a>,
    counters: EvaluationCounters,
    failures: Mutex<FailureSummary>,
}

impl CorridorEvaluator<'_> {
    pub async fn evaluate(&self, corridors: &[Corridor]) -> Result<Evaluation, RoutingError> {
        let started = std::time::Instant::now();
        let run = CorridorEvaluationRun::new(self);
        let journeys = run.evaluate(corridors).await?;
        let counters = &run.counters;
        Ok(Evaluation {
            stats: CorridorEvaluationStats {
                corridors_evaluated: counters.corridors.load(Ordering::Relaxed),
                states_expanded: counters.states.load(Ordering::Relaxed),
                successors_generated: counters.successors.load(Ordering::Relaxed),
                transit_departure_lookups: counters.departure_lookups.load(Ordering::Relaxed),
                final_candidates: journeys.len(),
                street_requests: self.matrix_cache.requests(),
                street_cache_hits: self.matrix_cache.hits(),
                latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                ..CorridorEvaluationStats::default()
            },
            failures: *run.failures.lock().unwrap_or_else(|e| e.into_inner()),
            journeys,
        })
    }
}

impl<'e, 'a> CorridorEvaluationRun<'e, 'a> {
    fn new(evaluator: &'e CorridorEvaluator<'a>) -> Self {
        Self {
            evaluator,
            counters: EvaluationCounters::default(),
            failures: Mutex::new(FailureSummary::default()),
        }
    }

    fn query(&self) -> &PlanningQuery {
        self.evaluator.query
    }

    fn check_deadline(&self) -> Result<(), RoutingError> {
        self.query().deadline.check()
    }

    fn record_failure(&self, error: &StreetRoutingError) {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_provider(error);
    }

    async fn evaluate(&self, corridors: &[Corridor]) -> Result<Vec<RealizedJourney>, RoutingError> {
        self.check_deadline()?;
        let concurrency = self
            .query()
            .limits
            .max_concurrent_phase2_corridors
            .clamp(1, 8);
        let evaluated = stream::iter(0..corridors.len())
            .map(|index| async move {
                let span = tracing::info_span!(
                    target: "via_profile",
                    "Evaluate Corridor",
                    corridor_index = index
                );
                let candidates = self
                    .evaluate_corridor(&corridors[index])
                    .instrument(span)
                    .await?;
                Ok::<_, RoutingError>((index, candidates))
            })
            .buffer_unordered(concurrency)
            .try_collect::<Vec<_>>();
        let candidates = self.query().deadline.run(evaluated).await??;
        self.check_deadline()?;
        let candidates =
            pareto_candidates(in_input_order(candidates).into_iter().flatten().collect());
        self.check_deadline()?;
        Ok(candidates.into_iter().map(|c| c.journey).collect())
    }

    async fn evaluate_corridor(
        &self,
        corridor: &Corridor,
    ) -> Result<Vec<EvaluatedCandidate>, RoutingError> {
        self.check_deadline()?;
        let mut states = vec![EvaluationState::initial(self.query())];
        for index in 0..corridor.edges().len() {
            let successors = self.expand_edge(&states, corridor, index).await?;
            states = prune_states(successors, self.query().limits.max_candidates);
            if states.is_empty() {
                break;
            }
        }
        self.counters.corridors.fetch_add(1, Ordering::Relaxed);
        self.finalize(corridor, states)
    }

    async fn expand_edge(
        &self,
        states: &[EvaluationState],
        corridor: &Corridor,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let concurrency = self
            .query()
            .limits
            .max_concurrent_phase2_states
            .clamp(1, 32);
        let expanded = stream::iter(0..states.len())
            .map(|position| async move {
                let successors = self
                    .expand_state(&states[position], corridor, index)
                    .await?;
                Ok::<_, RoutingError>((position, successors))
            })
            .buffer_unordered(concurrency)
            .try_collect::<Vec<_>>();
        let expanded = self.query().deadline.run(expanded).await??;
        Ok(in_input_order(expanded).into_iter().flatten().collect())
    }

    async fn expand_state(
        &self,
        state: &EvaluationState,
        corridor: &Corridor,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        self.check_deadline()?;
        self.counters.states.fetch_add(1, Ordering::Relaxed);
        let edge = &corridor.edges()[index];
        let mut successors = self
            .transit_successors(state, corridor, edge, index)
            .await?;
        successors.extend(self.walking_successor(state, corridor, edge, index).await?);
        successors.extend(self.bike_successors(state, corridor, index).await?);
        self.counters
            .successors
            .fetch_add(successors.len(), Ordering::Relaxed);
        Ok(successors)
    }

    async fn transit_successors(
        &self,
        state: &EvaluationState,
        corridor: &Corridor,
        edge: &PlannedMovement,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let PlannedMovement::Transit { legs } = edge else {
            return Ok(Vec::new());
        };
        let CorridorEvaluator {
            transit,
            timing,
            query,
            ..
        } = *self.evaluator;
        let ready_at = state.transit_ready_at(transit.same_stop_transfer_seconds());
        let mut successors = Vec::new();
        for leg in legs {
            self.check_deadline()?;
            validate_transit_leg(transit, leg)?;
            if !query.admits(ready_at) {
                continue;
            }
            let Some(hops) = self.leg_hops(corridor, leg, index).await? else {
                continue;
            };
            let leg_ready_at = ready_at + Duration::seconds(i64::from(hops.board_seconds));
            if !query.admits(leg_ready_at) {
                continue;
            }
            self.counters
                .departure_lookups
                .fetch_add(1, Ordering::Relaxed);
            for trip in timing.departures(
                &leg.pattern_id,
                leg.board_index,
                leg_ready_at,
                query.limits.max_departures_per_query,
            ) {
                self.check_deadline()?;
                successors.extend(transit_successor(
                    self.evaluator,
                    state,
                    trip,
                    leg,
                    index,
                    leg_ready_at,
                    hops,
                ));
            }
        }
        Ok(successors)
    }

    async fn leg_hops(
        &self,
        corridor: &Corridor,
        leg: &StaticTransitLeg,
        index: usize,
    ) -> Result<Option<LegHops>, RoutingError> {
        let anchors = corridor.anchors();
        let board = self
            .stop_hop_seconds(&anchors[index], &leg.board_stop_id, HopDirection::ToStop)
            .await?;
        let alight = self
            .stop_hop_seconds(
                &anchors[index + 1],
                &leg.alight_stop_id,
                HopDirection::FromStop,
            )
            .await?;
        Ok(board
            .zip(alight)
            .map(|(board_seconds, alight_seconds)| LegHops {
                board_seconds,
                alight_seconds,
            }))
    }

    async fn stop_hop_seconds(
        &self,
        anchor: &CorridorAnchor,
        stop_id: &BoardingPointId,
        direction: HopDirection,
    ) -> Result<Option<u32>, RoutingError> {
        if anchor.kind.boarding_point().is_none_or(|id| id == stop_id) {
            return Ok(Some(0));
        }
        let stop = self.evaluator.transit.stop(stop_id).ok_or_else(|| {
            RoutingError::Internal(format!(
                "retained transit leg references unknown stop {stop_id:?}"
            ))
        })?;
        let stop_coordinate = stop.coordinate.try_into().map_err(|_| {
            RoutingError::Internal(format!("stop {stop_id:?} has an invalid coordinate"))
        })?;
        let (from, to) = match direction {
            HopDirection::ToStop => (anchor.coordinate, stop_coordinate),
            HopDirection::FromStop => (stop_coordinate, anchor.coordinate),
        };
        let cell = walking_cell_between(self.evaluator.matrix_cache, from, to).await;
        self.check_deadline()?;
        match cell {
            Ok(MatrixCell::Reachable { duration, .. }) => Ok(Some(duration.0)),
            Ok(MatrixCell::Unreachable) => Ok(None),
            Err(error) => {
                self.record_failure(&error);
                Ok(None)
            }
        }
    }

    async fn walking_successor(
        &self,
        state: &EvaluationState,
        corridor: &Corridor,
        edge: &PlannedMovement,
        index: usize,
    ) -> Result<Option<EvaluationState>, RoutingError> {
        self.check_deadline()?;
        let (kind, duration_seconds, distance_metres) = match edge {
            PlannedMovement::RetainedWalking {
                kind,
                duration_seconds,
                distance_metres,
            } => (*kind, *duration_seconds, *distance_metres),
            PlannedMovement::Transit { .. } | PlannedMovement::WalkingToResolve => {
                let from = &corridor.anchors()[index];
                let to = &corridor.anchors()[index + 1];
                let cell = walking_cell(self.evaluator.matrix_cache, from, to).await;
                self.check_deadline()?;
                match cell {
                    Ok(MatrixCell::Reachable { duration, distance }) => {
                        (walking_kind(from, to), duration.0, distance.0)
                    }
                    Ok(MatrixCell::Unreachable) => return Ok(None),
                    Err(error) => {
                        self.record_failure(&error);
                        return Ok(None);
                    }
                }
            }
        };
        Ok(state.after_walk(
            self.query(),
            JourneyMovementKind::Walking {
                kind,
                duration_seconds,
                distance_metres,
            },
            duration_seconds,
            index,
        ))
    }

    async fn bike_successors(
        &self,
        state: &EvaluationState,
        corridor: &Corridor,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        self.check_deadline()?;
        let Some(bike_movements) = self.evaluator.bike_movements else {
            return Ok(Vec::new());
        };
        let departure = state.bike_departure(index);
        let anchors = corridor.anchors();
        let result = bike_movements
            .movements(&anchors[departure.start_anchor], &anchors[index + 1])
            .await;
        self.check_deadline()?;
        let movements = result.unwrap_or_else(|error| {
            self.record_failure(&error);
            Vec::new()
        });
        Ok(state.after_bike_shares(self.query(), movements, departure, index))
    }

    fn finalize(
        &self,
        corridor: &Corridor,
        states: Vec<EvaluationState>,
    ) -> Result<Vec<EvaluatedCandidate>, RoutingError> {
        let query = self.query();
        let mut candidates = Vec::new();
        for state in states {
            let metrics = state.metrics();
            if metrics.transfer_count as usize > query.limits.max_transfers {
                continue;
            }
            let mut reached_at = query.departure_time;
            let anchors = corridor
                .anchors()
                .iter()
                .enumerate()
                .map(|(index, anchor)| {
                    if let Some(movement) = state.movements.iter().find(|m| m.to_anchor == index) {
                        reached_at = movement.arrival_at;
                    }
                    JourneyAnchor {
                        kind: anchor.kind.clone(),
                        reached_at,
                    }
                })
                .collect();
            let journey =
                RealizedJourney::from_movements(anchors, state.movements, query.departure_time)
                    .map_err(|error| {
                        RoutingError::Internal(format!(
                            "evaluated journey has invalid movement continuity: {error}"
                        ))
                    })?;
            candidates.push(EvaluatedCandidate { metrics, journey });
        }
        Ok(candidates)
    }
}

fn in_input_order<T>(mut items: Vec<(usize, T)>) -> Vec<T> {
    items.sort_unstable_by_key(|(index, _)| *index);
    items.into_iter().map(|(_, item)| item).collect()
}

async fn walking_cell(
    matrix_cache: &MatrixCache,
    from: &CorridorAnchor,
    to: &CorridorAnchor,
) -> Result<MatrixCell, StreetRoutingError> {
    walking_cell_between(matrix_cache, from.coordinate, to.coordinate).await
}

async fn walking_cell_between(
    matrix_cache: &MatrixCache,
    from: Coordinate,
    to: Coordinate,
) -> Result<MatrixCell, StreetRoutingError> {
    matrix_cache
        .route_cell(RouteQuery {
            travel_mode: StreetTravelMode::Walking,
            origin: from,
            destination: to,
            constraints: RoutingConstraints::default(),
        })
        .await
}

#[derive(Clone, Copy)]
enum HopDirection {
    ToStop,
    FromStop,
}

#[derive(Clone, Copy, Default)]
struct LegHops {
    board_seconds: u32,
    alight_seconds: u32,
}

fn walking_kind(from: &CorridorAnchor, to: &CorridorAnchor) -> WalkingMovementKind {
    match (&from.kind, &to.kind) {
        (JourneyAnchorKind::Origin, JourneyAnchorKind::Destination) => WalkingMovementKind::Direct,
        (JourneyAnchorKind::Origin, _) => WalkingMovementKind::Access,
        (_, JourneyAnchorKind::Destination) => WalkingMovementKind::Egress,
        _ => WalkingMovementKind::Transfer,
    }
}

fn transit_successor(
    evaluator: &CorridorEvaluator,
    state: &EvaluationState,
    trip: &Arc<EffectiveTrip>,
    leg: &StaticTransitLeg,
    index: usize,
    ready_at: OffsetDateTime,
    hops: LegHops,
) -> Option<EvaluationState> {
    let arrival = alight_time(evaluator.transit, trip, leg, ready_at)?
        + Duration::seconds(i64::from(hops.alight_seconds));
    evaluator.query.admits(arrival).then(|| {
        state.after_movement(JourneyMovement {
            from_anchor: index,
            to_anchor: index + 1,
            departure_at: trip.stop_times[leg.board_index].departure_at,
            arrival_at: arrival,
            kind: JourneyMovementKind::Transit {
                trip: Arc::clone(trip),
                board_index: leg.board_index,
                alight_index: leg.alight_index,
                board_hop_seconds: hops.board_seconds,
                alight_hop_seconds: hops.alight_seconds,
            },
        })
    })
}

fn validate_transit_leg(
    transit: &TransitRuntime,
    leg: &StaticTransitLeg,
) -> Result<(), RoutingError> {
    let invalid = |reason: &str| {
        Err(RoutingError::Internal(format!(
            "retained transit leg {reason}"
        )))
    };
    if leg.board_index >= leg.alight_index {
        return invalid("has reversed stop indexes");
    }
    let pattern = transit.required_pattern(&leg.pattern_id);
    let Some(board_stop) = pattern.stops.get(leg.board_index) else {
        return invalid("has an invalid boarding index");
    };
    let Some(alight_stop) = pattern.stops.get(leg.alight_index) else {
        return invalid("has an invalid alighting index");
    };
    if board_stop != &leg.board_stop_id || alight_stop != &leg.alight_stop_id {
        return invalid("indexes do not match its stops");
    }
    Ok(())
}

fn alight_time(
    transit: &TransitRuntime,
    trip: &EffectiveTrip,
    leg: &StaticTransitLeg,
    ready_at: OffsetDateTime,
) -> Option<OffsetDateTime> {
    if trip.pattern_id != leg.pattern_id || leg.board_index >= leg.alight_index {
        return None;
    }
    let pattern = transit.pattern(&leg.pattern_id)?;
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
