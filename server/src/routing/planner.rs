use std::{sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::api::GeoCoordinate;
use crate::domain::{
    bikeshare::BikeShareNetwork,
    ids::BoardingPointId,
    transit::TransitMode,
    transit_runtime::TransitRuntime,
    transit_timing::{DepartureQuery, TimingSnapshotLoader},
};
use crate::providers::BikeAvailabilitySource;
use crate::street::{
    Coordinate, MatrixOrientation, MatrixQuery, RoutingConstraints, StreetRouter,
    StreetRoutingError, StreetTravelMode,
};

use super::{
    bike::{BikeShareMovementPlanner, BikeShareSession},
    corridor::EvaluationCorridor,
    evaluator::{self, Phase2EvaluationStats},
    model::{AccessPoint, EgressPoint, JourneyPlanningLimits, RoutingError, TransitQuery},
    static_search,
    surface::{SurfaceMovementPlanner, WalkingMovementPlanner},
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
    variant::JourneyVariant,
};

#[derive(Clone)]
pub struct TransitPlanner {
    network: Arc<TransitRuntime>,
    topology: Arc<TransitTopologyIndex>,
    transfers: Arc<TransferShortcutIndex>,
    timing: Arc<dyn TimingSnapshotLoader>,
    bike: Option<BikeShareMovementPlanner>,
    street: Option<Arc<dyn StreetRouter>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitPlannerStats {
    pub timing_snapshot_latency_ms: u64,
    pub phase2_evaluation: Phase2EvaluationStats,
    pub phase1: static_search::StaticSearchStats,
}

struct RequestSnapshots {
    timing: crate::domain::transit_timing::TimingSnapshot,
    bike: Option<Arc<crate::domain::bikeshare::BikeAvailabilitySnapshot>>,
    failures: evaluator::EvaluationFailures,
    latency_ms: u64,
}

impl TransitPlanner {
    pub fn new(network: Arc<TransitRuntime>, timing: Arc<dyn TimingSnapshotLoader>) -> Self {
        Self {
            topology: Arc::new(TransitTopologyIndex::new(network.network())),
            transfers: Arc::new(TransferShortcutIndex::from_network(network.network())),
            network,
            timing,
            bike: None,
            street: None,
        }
    }

    pub fn with_street_router(mut self, street_router: Arc<dyn StreetRouter>) -> Self {
        self.street = Some(street_router);
        self
    }

    pub fn with_bike_share(
        mut self,
        network: Arc<BikeShareNetwork>,
        availability_source: Arc<dyn BikeAvailabilitySource>,
        street_router: Arc<dyn StreetRouter>,
    ) -> Self {
        self.bike = Some(BikeShareMovementPlanner::new(
            network,
            availability_source,
            street_router,
        ));
        self
    }

    pub(crate) async fn discover_access_points(
        &self,
        street: &dyn StreetRouter,
        origin: GeoCoordinate,
        departure_time: OffsetDateTime,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<AccessPoint>, StreetRoutingError> {
        let origin = Coordinate::try_new(origin.latitude, origin.longitude)?;
        let stops = super::access::nearby_stops(
            &self.network,
            &self.topology,
            origin,
            allowed_transit_modes,
            limits.max_transit_access_stops,
            limits,
        );
        if stops.is_empty() {
            return Ok(Vec::new());
        }
        let matrix = street
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![origin],
                targets: stops.iter().map(|(_, coordinate)| *coordinate).collect(),
                constraints,
            })
            .await?;
        let mut result = matrix
            .cells
            .first()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, cell)| {
                let (stop_id, _) = stops.get(index)?;
                let metric = super::access::street_metric(cell)?;
                (metric.duration_seconds <= limits.max_walking_seconds).then(|| {
                    AccessPoint::new(
                        stop_id.clone(),
                        departure_time,
                        departure_time + Duration::seconds(i64::from(metric.duration_seconds)),
                        metric.duration_seconds,
                        metric.distance_metres,
                    )
                })
            })
            .collect::<Vec<_>>();
        result.sort_by(|left, right| {
            left.walking_seconds
                .cmp(&right.walking_seconds)
                .then(left.distance_metres.cmp(&right.distance_metres))
                .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
        });
        Ok(super::access::prune_access_coverage(
            &self.topology,
            result,
            limits.max_transit_access_stops,
        ))
    }

    pub(crate) async fn discover_egress_points(
        &self,
        street: &dyn StreetRouter,
        destination: GeoCoordinate,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<EgressPoint>, StreetRoutingError> {
        let destination = Coordinate::try_new(destination.latitude, destination.longitude)?;
        let stops = super::access::nearby_stops(
            &self.network,
            &self.topology,
            destination,
            allowed_transit_modes,
            limits.max_transit_egress_stops,
            limits,
        );
        if stops.is_empty() {
            return Ok(Vec::new());
        }
        let matrix = street
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToOne,
                travel_mode: StreetTravelMode::Walking,
                sources: stops.iter().map(|(_, coordinate)| *coordinate).collect(),
                targets: vec![destination],
                constraints,
            })
            .await?;
        let mut result = matrix
            .cells
            .iter()
            .enumerate()
            .filter_map(|(index, cells)| {
                let (stop_id, _) = stops.get(index)?;
                let metric = super::access::street_metric(cells.first()?)?;
                (metric.duration_seconds <= limits.max_walking_seconds).then(|| {
                    EgressPoint::new(
                        stop_id.clone(),
                        metric.duration_seconds,
                        metric.distance_metres,
                    )
                })
            })
            .collect::<Vec<_>>();
        result.sort_by(|left, right| {
            left.walking_seconds
                .cmp(&right.walking_seconds)
                .then(left.distance_metres.cmp(&right.distance_metres))
                .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
        });
        Ok(super::access::prune_egress_coverage(
            &self.topology,
            result,
            limits.max_transit_egress_stops,
        ))
    }

    pub(crate) fn modes_at_stop(&self, stop_id: &BoardingPointId) -> Vec<TransitMode> {
        super::access::modes_at_stop(&self.network, &self.topology, stop_id)
    }

    pub async fn plan_with_stats(
        &self,
        query: TransitQuery,
    ) -> Result<(Vec<JourneyVariant>, TransitPlannerStats), RoutingError> {
        let mut session = self.begin_plan(query).await?;
        let variants = session.evaluate(false).await?;
        Ok((variants, session.stats))
    }

    pub(crate) async fn begin_plan(
        &self,
        query: TransitQuery,
    ) -> Result<MovementSession<'_>, RoutingError> {
        if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
            return Err(RoutingError::DeadlineExceeded);
        }
        let (retained, static_stats) = if query.allowed_transit_modes.is_empty()
            || query.origin_access.is_empty()
            || query.destination_egress.is_empty()
        {
            (
                static_search::RetainedCorridorSet::default(),
                static_search::StaticSearchStats::default(),
            )
        } else {
            self.execute_phase1(&query)?
        };
        let loading = self.load_request_snapshots(&query, &retained, &static_stats);
        let snapshots = if let Some(deadline) = query.deadline {
            tokio::time::timeout_at(deadline.instant(), loading)
                .await
                .map_err(|_| RoutingError::DeadlineExceeded)??
        } else {
            loading.await?
        };
        let mut surface: Vec<Box<dyn SurfaceMovementPlanner>> = Vec::new();
        if query.origin.is_some()
            && query.destination.is_some()
            && let Some(street) = &self.street
        {
            surface.push(Box::new(WalkingMovementPlanner::new(street.clone())));
        }
        if let Some((bike, snapshot)) = self.bike.as_ref().zip(snapshots.bike.clone()) {
            surface.push(Box::new(BikeShareSession::new(
                bike.clone(),
                snapshot,
                OffsetDateTime::now_utc(),
            )));
        }
        Ok(MovementSession {
            network: &self.network,
            query,
            retained,
            snapshots,
            surface,
            stats: TransitPlannerStats {
                phase1: static_stats,
                ..TransitPlannerStats::default()
            },
            failures: evaluator::EvaluationFailures::default(),
            fallback_used: false,
        })
    }

    fn execute_phase1(
        &self,
        query: &TransitQuery,
    ) -> Result<
        (
            static_search::RetainedCorridorSet,
            static_search::StaticSearchStats,
        ),
        RoutingError,
    > {
        if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
            return Err(RoutingError::DeadlineExceeded);
        }
        if query.origin_access.is_empty() || query.destination_egress.is_empty() {
            return Err(RoutingError::InvalidQuery);
        }
        let max_legs = query.limits.max_transfers.saturating_add(1);
        let (corridor, static_stats) =
            static_search::generate(&self.network, &self.topology, &self.transfers, query);
        if static_stats.deadline_exceeded
            || query.deadline.is_some_and(|deadline| deadline.is_elapsed())
        {
            return Err(RoutingError::DeadlineExceeded);
        }

        tracing::debug!(
            target: "via_server::routing",
            static_search_states_expanded = static_stats.static_search_states_expanded,
            static_unattributed_states_expanded = static_stats.static_unattributed_states_expanded,
            static_pattern_scans = static_stats.static_pattern_scans,
            static_positions_scanned = static_stats.static_positions_scanned,
            static_walking_transfer_expansions = static_stats.static_walking_transfer_expansions,
            static_duplicate_states_rejected = static_stats.static_duplicate_states_rejected,
            static_dominance_states_rejected = static_stats.static_dominance_states_rejected,
            static_structurally_equivalent_states_rejected = static_stats.static_structurally_equivalent_states_rejected,
            static_equivalent_pattern_occurrences_collapsed = static_stats.static_equivalent_pattern_occurrences_collapsed,
            static_required_endpoint_mode_pairs = ?static_stats.static_required_endpoint_mode_pairs,
            static_complete_endpoint_mode_pairs = ?static_stats.static_complete_endpoint_mode_pairs,
            static_complete_candidates_found = static_stats.static_complete_candidates_found,
            static_unique_structural_corridors_found = static_stats.static_unique_structural_corridors_found,
            static_unique_corridors_found = static_stats.static_unique_corridors_found,
            static_corridors_retained = static_stats.static_corridors_retained,
            static_corridor_patterns = static_stats.static_corridor_patterns,
            static_corridor_positions = static_stats.static_corridor_positions,
            static_corridor_bus_stops = static_stats.static_corridor_bus_stops,
            static_search_latency_ms = static_stats.static_search_latency_ms,
            static_pattern_index_build_latency_us = static_stats.static_pattern_index_build_latency_us,
            static_pattern_index_pattern_edges_scanned = static_stats.static_pattern_index_pattern_edges_scanned,
            static_pattern_index_positions_scanned = static_stats.static_pattern_index_positions_scanned,
            static_ride_profile_build_latency_us = static_stats.static_ride_profile_build_latency_us,
            static_ride_profile_patterns_scanned = static_stats.static_ride_profile_patterns_scanned,
            static_ride_profile_positions_scanned = static_stats.static_ride_profile_positions_scanned,
            static_lower_bound_latency_us = static_stats.static_lower_bound_latency_us,
            static_lower_bound_nodes_settled = static_stats.static_lower_bound_nodes_settled,
            static_lower_bound_pattern_edges_scanned = static_stats.static_lower_bound_pattern_edges_scanned,
            static_lower_bound_transfer_edges_scanned = static_stats.static_lower_bound_transfer_edges_scanned,
            static_forward_search_latency_us = static_stats.static_forward_search_latency_us,
            static_retention_latency_us = static_stats.static_retention_latency_us,
            static_mode_stats = ?static_stats.static_mode_stats,
            static_expanded_stop_occurrence_samples = static_stats.static_expanded_stop_occurrence_samples,
            static_expanded_stop_occurrences_average_milli = static_stats.static_expanded_stop_occurrences_average_milli,
            static_expanded_stop_occurrences_max = static_stats.static_expanded_stop_occurrences_max,
            static_expanded_stop_pattern_occurrences_average_milli = static_stats.static_expanded_stop_pattern_occurrences_average_milli,
            static_expanded_stop_pattern_occurrences_max = static_stats.static_expanded_stop_pattern_occurrences_max,
            static_expanded_egress_states = static_stats.static_expanded_egress_states,
            static_expanded_terminal_transfer_source_states = static_stats.static_expanded_terminal_transfer_source_states,
            static_expanded_states_by_leg_count = ?static_stats.static_expanded_states_by_leg_count,
            static_most_expanded_stops = ?static_stats.static_most_expanded_stops,
            kth_static_duration_seconds = ?static_stats.kth_static_duration_seconds,
            exact_schedule_cost = static_stats.exact_schedule_cost,
            derived_bus_cost = static_stats.derived_bus_cost,
            estimated_cost = static_stats.estimated_cost,
            mixed_cost = static_stats.mixed_cost,
            static_search_limit_reached = static_stats.static_search_limit_reached,
            static_top_k_early_terminated = static_stats.static_top_k_early_terminated,
            max_legs,
            "static Top-K corridor search"
        );
        tracing::debug!(
            target: "via_server::routing",
            pattern_sequence_group_count = static_stats.static_retained_pattern_sequence_groups.len(),
            retained_pattern_sequence_groups = ?static_stats.static_retained_pattern_sequence_groups,
            "retained static corridor sequence groups"
        );
        tracing::debug!(
            target: "via_server::routing",
            retained_mode_sequence_groups = ?static_stats.static_retained_mode_sequence_groups,
            "retained static corridor mode-sequence groups"
        );
        for (index, retained) in corridor.retained().iter().enumerate() {
            tracing::debug!(
                target: "via_server::routing",
                corridor_rank = index + 1,
                theoretical_duration_seconds = retained.theoretical_duration_seconds,
                pattern_sequence = ?retained.pattern_sequence(),
                structural_steps = ?retained.steps,
                cost_source = retained.cost_source.as_str(),
                "retained static corridor"
            );
        }
        if static_stats.static_search_limit_reached {
            tracing::warn!(
                target: "via_server::routing",
                max_static_search_expansions = query.limits.max_phase1_expansions,
                "static Top-K search hit its safety bound; rejecting the partial corridor set"
            );
            return Err(RoutingError::StaticSearchLimit);
        }
        Ok((corridor, static_stats))
    }

    async fn load_request_snapshots(
        &self,
        query: &TransitQuery,
        corridor: &static_search::RetainedCorridorSet,
        static_stats: &static_search::StaticSearchStats,
    ) -> Result<RequestSnapshots, RoutingError> {
        let positions = corridor.timing_positions().collect::<Vec<_>>();
        let snapshot_limit = usize::try_from(query.limits.max_journey_duration_seconds)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .max(query.limits.max_departures_per_query);
        let timing_queries = positions
            .into_iter()
            .filter_map(|position| {
                let pattern = self.network.pattern(&position.pattern_id)?;
                let stop_id = pattern.stops.get(position.stop_index)?.clone();
                Some(
                    DepartureQuery::at_position(
                        position.pattern_id,
                        stop_id,
                        position.stop_index,
                        query.departure_time,
                        snapshot_limit,
                    )
                    .until(
                        query.departure_time
                            + Duration::seconds(i64::from(
                                query.limits.max_journey_duration_seconds,
                            )),
                    ),
                )
            })
            .collect::<Vec<_>>();
        let timing_positions_requested = timing_queries.len();
        let snapshot_started = Instant::now();
        let bike_enabled = query.allow_bike_share && self.bike.is_some();
        let mut failures = evaluator::EvaluationFailures::default();
        let (snapshot, bike_snapshot) = if bike_enabled {
            let bike = self.bike.as_ref().expect("bike planner checked above");

            let (snapshot, bike_snapshot) = tokio::join!(
                async {
                    if timing_queries.is_empty() {
                        crate::domain::transit_timing::TimingSnapshot::default()
                    } else {
                        self.timing.load_snapshot(timing_queries).await
                    }
                },
                bike.availability_source.snapshot()
            );
            let bike_snapshot = match bike_snapshot {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    failures.transient |= error.is_transient();
                    failures.internal |= !error.is_transient();
                    tracing::debug!(
                        target: "via_server::routing",
                        error = %error,
                        "BikeShare availability unavailable; retaining transit baseline"
                    );
                    None
                }
            };
            (snapshot, bike_snapshot)
        } else {
            (
                async {
                    if timing_queries.is_empty() {
                        crate::domain::transit_timing::TimingSnapshot::default()
                    } else {
                        self.timing.load_snapshot(timing_queries).await
                    }
                }
                .await,
                None,
            )
        };
        if query.deadline.is_some_and(|deadline| deadline.is_elapsed()) {
            return Err(RoutingError::DeadlineExceeded);
        }
        tracing::debug!(
            target: "via_server::routing",
            positions_requested = timing_positions_requested,
            snapshot_summary = ?snapshot.trace_summary(),
            relevant_bus_stops = static_stats.static_corridor_bus_stops,
            bike_share_enabled = bike_enabled,
            latency_ms = snapshot_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "timing snapshot"
        );
        let latency_ms = snapshot_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if snapshot.all_positions_failed() {
            failures.transient |= snapshot
                .first_failure()
                .is_some_and(|error| error.is_transient());
            failures.internal |= snapshot
                .first_failure()
                .is_some_and(|error| !error.is_transient());
        }
        Ok(RequestSnapshots {
            timing: snapshot,
            bike: bike_snapshot,
            failures,
            latency_ms,
        })
    }
}

/// Request-local snapshots, movement caches and expansion accounting survive reconstruction.
/// A failed reconstruction can therefore use the same evaluator capabilities for fallback.
pub(crate) struct MovementSession<'a> {
    network: &'a TransitRuntime,
    query: TransitQuery,
    retained: static_search::RetainedCorridorSet,
    snapshots: RequestSnapshots,
    surface: Vec<Box<dyn SurfaceMovementPlanner>>,
    pub stats: TransitPlannerStats,
    pub failures: evaluator::EvaluationFailures,
    pub fallback_used: bool,
}

impl MovementSession<'_> {
    pub async fn evaluate(&mut self, fallback: bool) -> Result<Vec<JourneyVariant>, RoutingError> {
        let corridors = if fallback {
            Vec::new()
        } else {
            self.retained
                .retained()
                .iter()
                .filter_map(|corridor| {
                    EvaluationCorridor::from_transit(self.network, corridor, &self.query)
                })
                .collect::<Vec<_>>()
        };
        let mut evaluator = evaluator::FixedCorridorEvaluator::new(
            self.network,
            &self.retained,
            &self.snapshots.timing,
            &self.query,
            &mut self.surface,
        );
        evaluator.stats = self.stats.phase2_evaluation;
        evaluator.failures = self.failures;
        let evaluation = async {
            let mut result = evaluator.evaluate(&corridors).await?;
            if result.variants.is_empty()
                && !self.fallback_used
                && let Some((origin, destination)) = self.query.origin.zip(self.query.destination)
            {
                self.fallback_used = true;
                result = evaluator
                    .evaluate(&[EvaluationCorridor::trivial(origin, destination)])
                    .await?;
            }
            Ok::<_, RoutingError>(result)
        };
        let result = if let Some(deadline) = self.query.deadline {
            tokio::time::timeout_at(deadline.instant(), evaluation)
                .await
                .map_err(|_| RoutingError::DeadlineExceeded)??
        } else {
            evaluation.await?
        };
        self.stats.timing_snapshot_latency_ms = self.snapshots.latency_ms;
        self.stats.phase2_evaluation = result.stats;
        self.failures = result.failures;
        self.failures.transient |= self.snapshots.failures.transient;
        self.failures.internal |= self.snapshots.failures.internal;
        Ok(result.variants)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use time::{Time, Weekday, macros::datetime};

    use super::*;
    use crate::{
        api::GeoCoordinate,
        domain::{
            bikeshare::{BikeAvailabilitySnapshot, BikeShareNetwork},
            ids::{BoardingPointId, EffectiveTripId, TransitPatternId, TransitTripId},
            transit::{
                ScheduledStopTime, TransitMode, TransitNetwork, TransitPattern, TransitStop,
                TransitTrip,
            },
            transit_runtime::TransitRuntime,
            transit_timing::{
                EffectiveStopTime, EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot,
            },
        },
        providers::{BikeAvailabilityError, BikeAvailabilitySource},
        routing::{AccessPoint, EgressPoint},
        street::FakeStreetRouter,
    };

    #[derive(Default)]
    struct RecordingTimingLoader {
        queries: Mutex<Vec<DepartureQuery>>,
        calls: AtomicUsize,
        trips: HashMap<TransitPatternId, Vec<EffectiveTrip>>,
    }

    #[async_trait::async_trait]
    impl TimingSnapshotLoader for RecordingTimingLoader {
        async fn load_snapshot(&self, queries: Vec<DepartureQuery>) -> TimingSnapshot {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.queries.lock().unwrap() = queries.clone();
            let mut snapshot = TimingSnapshot::default();
            for query in queries {
                snapshot.insert(
                    query.pattern_id.clone(),
                    query.board_index.unwrap(),
                    self.trips
                        .get(&query.pattern_id)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            snapshot
        }
    }

    struct CountingBikeAvailabilitySource {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl BikeAvailabilitySource for CountingBikeAvailabilitySource {
        async fn snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(BikeAvailabilitySnapshot::new(
                datetime!(2026-09-21 10:00 UTC),
                HashMap::new(),
            )))
        }
    }

    fn stop(value: &str) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(value),
            value,
            GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )
    }

    fn pattern(value: &str) -> TransitPattern {
        TransitPattern::fixture(
            TransitPatternId::new(value),
            TransitMode::Bus,
            value,
            "destination",
            vec![
                BoardingPointId::new("origin"),
                BoardingPointId::new("destination"),
            ],
        )
    }

    fn scheduled_trip(value: &str, pattern_id: &str, duration: u32) -> TransitTrip {
        TransitTrip::fixture_weekdays(
            TransitTripId::new(value),
            TransitPatternId::new(pattern_id),
            vec![Weekday::Monday],
            vec![
                ScheduledStopTime::new(0, 0),
                ScheduledStopTime::new(duration, duration),
            ],
        )
    }

    fn effective_trip(value: &str, pattern_id: &str, arrival_minute: i64) -> EffectiveTrip {
        EffectiveTrip {
            id: EffectiveTripId::new(value),
            pattern_id: TransitPatternId::new(pattern_id),
            scheduled_trip_id: None,
            stop_times: vec![
                EffectiveStopTime {
                    stop_id: BoardingPointId::new("origin"),
                    arrival_at: datetime!(2026-09-21 10:01 UTC),
                    departure_at: datetime!(2026-09-21 10:01 UTC),
                },
                EffectiveStopTime {
                    stop_id: BoardingPointId::new("destination"),
                    arrival_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(arrival_minute),
                    departure_at: datetime!(2026-09-21 10:00 UTC)
                        + Duration::minutes(arrival_minute),
                },
            ],
            timing_basis: TimingBasis::RealtimePrediction,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        }
    }

    fn network() -> Arc<TransitRuntime> {
        let network = TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("origin"), stop("destination")],
            vec![pattern("static-fast"), pattern("realtime-fast")],
            vec![
                scheduled_trip("static-fast-trip", "static-fast", 100),
                scheduled_trip("realtime-fast-trip", "realtime-fast", 200),
            ],
        );
        Arc::new(TransitRuntime::new(Arc::new(network)))
    }

    fn multiple_mode_corridors_network() -> Arc<TransitRuntime> {
        let pattern = |id: &str, mode, from: &str, to: &str| {
            TransitPattern::fixture(
                TransitPatternId::new(id),
                mode,
                id,
                to,
                vec![BoardingPointId::new(from), BoardingPointId::new(to)],
            )
        };
        let network = TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            [
                "origin",
                "bus-end-a",
                "metro-start-a",
                "bus-end-b",
                "metro-start-b",
                "destination",
            ]
            .into_iter()
            .map(stop)
            .collect(),
            vec![
                pattern("bus-a", TransitMode::Bus, "origin", "bus-end-a"),
                pattern(
                    "metro-a",
                    TransitMode::Metro,
                    "metro-start-a",
                    "destination",
                ),
                pattern("bus-b", TransitMode::Bus, "origin", "bus-end-b"),
                pattern(
                    "metro-b",
                    TransitMode::Metro,
                    "metro-start-b",
                    "destination",
                ),
            ],
            Vec::new(),
        )
        .with_transfer_edges(vec![
            crate::domain::transit::TransferEdge::new(
                BoardingPointId::new("bus-end-a"),
                BoardingPointId::new("metro-start-a"),
                60,
                100,
            ),
            crate::domain::transit::TransferEdge::new(
                BoardingPointId::new("bus-end-b"),
                BoardingPointId::new("metro-start-b"),
                60,
                100,
            ),
        ]);
        Arc::new(TransitRuntime::new(Arc::new(network)))
    }

    fn query(k: usize) -> TransitQuery {
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
        .with_allowed_transit_modes([TransitMode::Bus])
        .with_limits(crate::routing::PlannerLimits {
            max_transfers: 0,
            max_candidates: 16,
            max_phase2_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
            ..crate::routing::PlannerLimits::default()
        })
        .with_max_static_corridors(k)
    }

    #[tokio::test]
    async fn timing_queries_include_all_patterns_in_a_retained_pattern_family() {
        let loader = Arc::new(RecordingTimingLoader {
            trips: HashMap::from([
                (
                    TransitPatternId::new("static-fast"),
                    vec![effective_trip("static-fast-live", "static-fast", 20)],
                ),
                (
                    TransitPatternId::new("realtime-fast"),
                    vec![effective_trip("realtime-fast-live", "realtime-fast", 10)],
                ),
            ]),
            ..RecordingTimingLoader::default()
        });
        let planner = TransitPlanner::new(network(), loader.clone());

        let (_, stats) = planner.plan_with_stats(query(1)).await.unwrap();

        let queries = loader.queries.lock().unwrap();
        assert_eq!(queries.len(), 2);
        assert!(
            queries
                .iter()
                .any(|query| query.pattern_id == TransitPatternId::new("static-fast"))
        );
        assert!(
            queries
                .iter()
                .any(|query| query.pattern_id == TransitPatternId::new("realtime-fast"))
        );
        assert_eq!(stats.phase1.static_corridor_positions, 2);
        assert_eq!(stats.phase1.static_corridors_retained, 1);
    }

    #[tokio::test]
    async fn expired_deadline_stops_before_loading_timing_snapshot() {
        let loader = Arc::new(RecordingTimingLoader::default());
        let planner = TransitPlanner::new(network(), loader.clone());
        let query =
            query(1).with_deadline(super::super::model::PlanningDeadline::elapsed_for_test());

        let result = planner.plan_with_stats(query).await;

        assert_eq!(result.unwrap_err(), RoutingError::DeadlineExceeded);
        assert_eq!(loader.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn phase2_chooses_actual_best_journey_within_retained_corridors() {
        let loader = Arc::new(RecordingTimingLoader {
            trips: HashMap::from([
                (
                    TransitPatternId::new("static-fast"),
                    vec![effective_trip("static-fast-live", "static-fast", 20)],
                ),
                (
                    TransitPatternId::new("realtime-fast"),
                    vec![effective_trip("realtime-fast-live", "realtime-fast", 10)],
                ),
            ]),
            ..RecordingTimingLoader::default()
        });
        let planner = TransitPlanner::new(network(), loader);

        let (candidates, stats) = planner.plan_with_stats(query(2)).await.unwrap();

        assert_eq!(stats.phase1.static_corridors_retained, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![TransitPatternId::new("realtime-fast")]
        );
    }

    #[tokio::test]
    async fn phase2_expansion_limit_is_returned_to_the_planner_caller() {
        let loader = Arc::new(RecordingTimingLoader {
            trips: HashMap::from([(
                TransitPatternId::new("static-fast"),
                vec![effective_trip("static-fast-live", "static-fast", 20)],
            )]),
            ..RecordingTimingLoader::default()
        });
        let planner = TransitPlanner::new(network(), loader);
        let mut query = query(1);
        query.origin_access.push(query.origin_access[0].clone());
        query.limits.max_phase2_expansions = 1;

        assert!(matches!(
            planner.plan_with_stats(query).await,
            Err(RoutingError::Phase2EvaluationLimit)
        ));
    }

    #[tokio::test]
    async fn bike_availability_snapshot_is_loaded_once_per_planning_request() {
        let timing = Arc::new(RecordingTimingLoader::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let planner = TransitPlanner::new(multiple_mode_corridors_network(), timing.clone())
            .with_bike_share(
                Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
                Arc::new(CountingBikeAvailabilitySource {
                    calls: calls.clone(),
                }),
                Arc::new(FakeStreetRouter::builder().build()),
            );
        let query = query(2)
            .with_allowed_transit_modes([TransitMode::Bus, TransitMode::Metro])
            .with_limits(crate::routing::PlannerLimits {
                max_transfers: 1,
                max_candidates: 16,
                max_phase2_expansions: 2_000,
                max_departures_per_query: 3,
                max_journey_duration_seconds: 4 * 60 * 60,
                ..crate::routing::PlannerLimits::default()
            })
            .with_bike_share(true);

        let (_, stats) = planner.plan_with_stats(query).await.unwrap();

        assert_eq!(stats.phase1.static_corridors_retained, 2);
        assert_eq!(timing.calls.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn walking_is_configured_independently_of_bike_share() {
        let timing = Arc::new(RecordingTimingLoader::default());
        let origin = Coordinate::try_new(25.0, 121.5).unwrap();
        let destination = Coordinate::try_new(25.001, 121.501).unwrap();
        let street = Arc::new(
            FakeStreetRouter::builder()
                .path(
                    crate::street::StreetTravelMode::Walking,
                    origin,
                    destination,
                    crate::street::StreetPath {
                        duration: crate::street::Seconds(60),
                        distance: crate::street::Metres(100),
                        geometry: vec![origin, destination],
                        maneuvers: Vec::new(),
                        attribution: Vec::new(),
                    },
                )
                .build(),
        );
        let planner = TransitPlanner::new(network(), timing.clone()).with_street_router(street);
        let query = TransitQuery::new(Vec::new(), Vec::new(), datetime!(2026-09-21 10:00 UTC))
            .with_allowed_transit_modes([])
            .with_endpoints(origin, destination);
        let (variants, stats) = planner.plan_with_stats(query).await.unwrap();
        assert_eq!(variants.len(), 1);
        assert_eq!(stats.phase1.static_search_states_expanded, 0);
        assert_eq!(timing.calls.load(Ordering::SeqCst), 0);
    }
}
