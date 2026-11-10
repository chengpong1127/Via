pub(crate) mod bike;
mod corridor;
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

pub(crate) use corridor::EvaluationCorridor;

use super::{
    cache::MatrixCache,
    corridors::{RetainedCorridorSet, StaticTransitLeg},
    journey::{
        BikeShareMovement, JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
        RealizedJourney, WalkingMovementKind,
    },
    model::{FailureSummary, PlanningQuery, RoutingError},
};
use crate::{
    domain::{
        realtime::NormalizedRealtimeValue,
        transit_runtime::TransitRuntime,
        transit_timing::{EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot},
    },
    street::{
        MatrixCell, MatrixOrientation, MatrixQuery, RoutingConstraints, StreetRouter,
        StreetRoutingError, StreetTravelMode,
    },
};
use bike::BikeMovementSource;
use corridor::{CorridorAnchor, PlannedMovement};
use pareto::{EvaluatedCandidate, pareto_candidates, prune_states};
use state::{EvaluationState, PathMetrics};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CorridorEvaluationStats {
    pub corridors_evaluated: usize,
    pub states_expanded: usize,
    pub successors_generated: usize,
    pub transit_departure_lookups: usize,
    pub final_candidates: usize,
    pub matrix_requests: usize,
    pub matrix_cache_hits: usize,
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
    pub network: &'a TransitRuntime,
    pub retained: &'a RetainedCorridorSet,
    pub timing: &'a TimingSnapshot,
    pub query: &'a PlanningQuery,
    pub street: &'a MatrixCache,
    pub bike: Option<&'a dyn BikeMovementSource>,
}

#[derive(Default)]
struct Counters {
    corridors: AtomicUsize,
    states: AtomicUsize,
    successors: AtomicUsize,
    departure_lookups: AtomicUsize,
}

struct Run<'e, 'a> {
    evaluator: &'e CorridorEvaluator<'a>,
    counters: Counters,
    failures: Mutex<FailureSummary>,
}

impl CorridorEvaluator<'_> {
    pub async fn evaluate(
        &self,
        corridors: &[EvaluationCorridor],
    ) -> Result<Evaluation, RoutingError> {
        let started = std::time::Instant::now();
        let run = Run::new(self);
        let journeys = run.evaluate(corridors).await?;
        let counters = &run.counters;
        Ok(Evaluation {
            stats: CorridorEvaluationStats {
                corridors_evaluated: counters.corridors.load(Ordering::Relaxed),
                states_expanded: counters.states.load(Ordering::Relaxed),
                successors_generated: counters.successors.load(Ordering::Relaxed),
                transit_departure_lookups: counters.departure_lookups.load(Ordering::Relaxed),
                final_candidates: journeys.len(),
                matrix_requests: self.street.requests(),
                matrix_cache_hits: self.street.hits(),
                latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                ..CorridorEvaluationStats::default()
            },
            failures: *run.failures.lock().unwrap_or_else(|e| e.into_inner()),
            journeys,
        })
    }
}

impl<'e, 'a> Run<'e, 'a> {
    fn new(evaluator: &'e CorridorEvaluator<'a>) -> Self {
        Self {
            evaluator,
            counters: Counters::default(),
            failures: Mutex::new(FailureSummary::default()),
        }
    }

    fn query(&self) -> &PlanningQuery {
        self.evaluator.query
    }

    fn check_deadline(&self) -> Result<(), RoutingError> {
        if self.query().deadline.is_elapsed() {
            Err(RoutingError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }

    fn record_failure(&self, error: &StreetRoutingError) {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_provider(error);
    }

    async fn evaluate(
        &self,
        corridors: &[EvaluationCorridor],
    ) -> Result<Vec<RealizedJourney>, RoutingError> {
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
        corridor: &EvaluationCorridor,
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
        corridor: &EvaluationCorridor,
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
        corridor: &EvaluationCorridor,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        self.check_deadline()?;
        self.counters.states.fetch_add(1, Ordering::Relaxed);
        let edge = &corridor.edges()[index];
        let mut successors = self.transit_successors(state, edge, index)?;
        successors.extend(self.walking_successor(state, corridor, edge, index).await?);
        successors.extend(self.bike_successors(state, corridor, index).await?);
        self.counters
            .successors
            .fetch_add(successors.len(), Ordering::Relaxed);
        Ok(successors)
    }

    fn transit_successors(
        &self,
        state: &EvaluationState,
        edge: &PlannedMovement,
        anchor: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let PlannedMovement::Transit(leg) = edge else {
            return Ok(Vec::new());
        };
        let CorridorEvaluator {
            network,
            retained,
            timing,
            query,
            ..
        } = *self.evaluator;
        let ready_at = transit_ready_at(state, network.same_stop_transfer_seconds());
        let mut successors = Vec::new();
        for equivalent_leg in retained.equivalent_legs_for(network, leg) {
            self.check_deadline()?;
            validate_equivalent_leg(network, retained, leg, &equivalent_leg)?;
            if !within_query_bounds(query, ready_at) {
                continue;
            }
            self.counters
                .departure_lookups
                .fetch_add(1, Ordering::Relaxed);
            for trip in timing.departures(
                &equivalent_leg.pattern_id,
                equivalent_leg.board_index,
                ready_at,
                query.limits.max_departures_per_query,
            ) {
                self.check_deadline()?;
                successors.extend(transit_successor(
                    network,
                    query,
                    state,
                    trip,
                    &equivalent_leg,
                    anchor,
                    ready_at,
                ));
            }
        }
        Ok(successors)
    }

    async fn walking_successor(
        &self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &PlannedMovement,
        index: usize,
    ) -> Result<Option<EvaluationState>, RoutingError> {
        self.check_deadline()?;
        let walking = match edge {
            PlannedMovement::RetainedWalking {
                kind,
                duration_seconds,
                distance_metres,
                access_available_at,
            } => Walking {
                kind: *kind,
                duration_seconds: *duration_seconds,
                distance_metres: *distance_metres,
                access_available_at: *access_available_at,
            },
            PlannedMovement::Transit(_) | PlannedMovement::WalkingToResolve => {
                let from = &corridor.anchors()[index];
                let to = &corridor.anchors()[index + 1];
                let cell = walking_cell(self.evaluator.street, from, to).await;
                self.check_deadline()?;
                match cell {
                    Ok(MatrixCell::Reachable { duration, distance }) => Walking {
                        kind: walking_kind(from, to),
                        duration_seconds: duration.0,
                        distance_metres: distance.0,
                        access_available_at: None,
                    },
                    Ok(MatrixCell::Unreachable) => return Ok(None),
                    Err(error) => {
                        self.record_failure(&error);
                        return Ok(None);
                    }
                }
            }
        };
        Ok(walking_successor(self.query(), state, walking, index))
    }

    async fn bike_successors(
        &self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        self.check_deadline()?;
        let Some(bike) = self.evaluator.bike else {
            return Ok(Vec::new());
        };
        let (start_anchor, _) = state.bike_departure(index);
        let anchors = corridor.anchors();
        let result = bike
            .movements(
                &anchors[start_anchor],
                &anchors[index + 1],
                start_anchor,
                index + 1,
            )
            .await;
        self.check_deadline()?;
        let movements = result.unwrap_or_else(|error| {
            self.record_failure(&error);
            Vec::new()
        });
        Ok(bike_successors(self.query(), state, movements, index))
    }

    fn finalize(
        &self,
        corridor: &EvaluationCorridor,
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
    street: &dyn StreetRouter,
    from: &CorridorAnchor,
    to: &CorridorAnchor,
) -> Result<MatrixCell, StreetRoutingError> {
    let matrix = street
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            travel_mode: StreetTravelMode::Walking,
            sources: vec![from.coordinate],
            targets: vec![to.coordinate],
            constraints: RoutingConstraints::default(),
        })
        .await?;
    match matrix.cells.as_slice() {
        [row] if row.len() == 1 => Ok(row[0]),
        _ => Err(StreetRoutingError::MalformedResponse),
    }
}

fn walking_kind(from: &CorridorAnchor, to: &CorridorAnchor) -> WalkingMovementKind {
    match (&from.kind, &to.kind) {
        (JourneyAnchorKind::Origin, JourneyAnchorKind::Destination) => WalkingMovementKind::Direct,
        (JourneyAnchorKind::Origin, _) => WalkingMovementKind::Access,
        (_, JourneyAnchorKind::Destination) => WalkingMovementKind::Egress,
        _ => WalkingMovementKind::Transfer,
    }
}

fn within_query_bounds(query: &PlanningQuery, time: OffsetDateTime) -> bool {
    time >= query.departure_time
        && time - query.departure_time
            <= Duration::seconds(i64::from(query.limits.max_journey_duration_seconds))
}

fn transit_ready_at(state: &EvaluationState, transfer_seconds: u32) -> OffsetDateTime {
    if state.last_was_transit {
        state.ready_at + Duration::seconds(i64::from(transfer_seconds))
    } else {
        state.ready_at
    }
}

fn transit_successor(
    network: &TransitRuntime,
    query: &PlanningQuery,
    state: &EvaluationState,
    trip: &EffectiveTrip,
    leg: &StaticTransitLeg,
    anchor: usize,
    ready_at: OffsetDateTime,
) -> Option<EvaluationState> {
    let arrival = alight_time(network, trip, leg, ready_at)?;
    within_query_bounds(query, arrival).then(|| {
        state.after_movement(JourneyMovement {
            from_anchor: anchor,
            to_anchor: anchor + 1,
            departure_at: trip.stop_times[leg.board_index].departure_at,
            arrival_at: arrival,
            kind: JourneyMovementKind::Transit {
                trip: Arc::new(trip.clone()),
                board_index: leg.board_index,
                alight_index: leg.alight_index,
            },
        })
    })
}

struct Walking {
    kind: WalkingMovementKind,
    duration_seconds: u32,
    distance_metres: u32,
    access_available_at: Option<OffsetDateTime>,
}

fn walking_successor(
    query: &PlanningQuery,
    state: &EvaluationState,
    walking: Walking,
    index: usize,
) -> Option<EvaluationState> {
    let arrival = walking
        .access_available_at
        .unwrap_or(state.ready_at + Duration::seconds(i64::from(walking.duration_seconds)));
    within_query_bounds(query, arrival).then(|| {
        state.after_movement(JourneyMovement {
            from_anchor: index,
            to_anchor: index + 1,
            departure_at: state.ready_at,
            arrival_at: arrival,
            kind: JourneyMovementKind::Walking {
                kind: walking.kind,
                duration_seconds: walking.duration_seconds,
                distance_metres: walking.distance_metres,
            },
        })
    })
}

fn bike_successors(
    query: &PlanningQuery,
    state: &EvaluationState,
    movements: Vec<BikeShareMovement>,
    index: usize,
) -> Vec<EvaluationState> {
    let (start_anchor, started_at) = state.bike_departure(index);
    movements
        .into_iter()
        .filter_map(|movement| {
            let arrival = started_at + Duration::seconds(i64::from(movement.total_seconds));
            if !within_query_bounds(query, arrival) {
                return None;
            }
            state.after_bike_share(
                movement,
                start_anchor,
                index + 1,
                started_at,
                arrival,
                query.departure_time,
            )
        })
        .collect()
}

fn validate_equivalent_leg(
    network: &TransitRuntime,
    retained: &RetainedCorridorSet,
    leg: &StaticTransitLeg,
    equivalent_leg: &StaticTransitLeg,
) -> Result<(), RoutingError> {
    let invalid = |reason: &str| {
        Err(RoutingError::Internal(format!(
            "retained transit leg {reason}"
        )))
    };
    if !retained.is_equivalent_leg(network, leg, equivalent_leg) {
        return invalid("has an invalid equivalent leg");
    }
    if equivalent_leg.board_index >= equivalent_leg.alight_index {
        return invalid("has reversed stop indexes");
    }
    let pattern = network.required_pattern(&equivalent_leg.pattern_id);
    let Some(board_stop) = pattern.stops.get(equivalent_leg.board_index) else {
        return invalid("has an invalid boarding index");
    };
    let Some(alight_stop) = pattern.stops.get(equivalent_leg.alight_index) else {
        return invalid("has an invalid alighting index");
    };
    if board_stop != &leg.board_stop_id || alight_stop != &leg.alight_stop_id {
        return invalid("indexes do not match its stops");
    }
    Ok(())
}

fn alight_time(
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
