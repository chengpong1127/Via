//! Canonical two-phase transit and BikeShare planner.

use std::{sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::domain::{
    bikeshare::BikeShareNetwork,
    transit_runtime::TransitRuntime,
    transit_timing::{DepartureQuery, TimingSnapshotLoader},
};
use crate::{providers::BikeAvailabilitySource, street::StreetRouter};

use super::{
    bike::{BikePlanningStats, BikeSegmentPlanner},
    evaluator::{self, Phase2EvaluationStats},
    model::{RoutingError, TransitQuery},
    path::JourneyVariant,
    static_search::{self, StaticCorridorStep},
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
};

#[derive(Clone)]
pub struct TransitPlanner {
    network: Arc<TransitRuntime>,
    topology: Arc<TransitTopologyIndex>,
    transfers: Arc<TransferShortcutIndex>,
    timing: Arc<dyn TimingSnapshotLoader>,
    bike: Option<BikeSegmentPlanner>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitPlannerStats {
    pub timing_snapshot_latency_ms: u64,
    pub phase2_evaluation: Phase2EvaluationStats,
    pub phase1: static_search::StaticSearchStats,
    pub bike_share_enabled: bool,
    pub relevant_bike_stations: usize,
    pub bike_candidate_movements: usize,
    pub bike_pickup_stations_considered: usize,
    pub bike_dropoff_stations_considered: usize,
    pub bike_station_pairs: usize,
    pub bike_matrix_requests: usize,
    pub bike_realizations_feasible: usize,
    pub multimodal_variants: usize,
}

impl TransitPlanner {
    pub(crate) fn topology(&self) -> &TransitTopologyIndex {
        &self.topology
    }

    pub fn new(network: Arc<TransitRuntime>, timing: Arc<dyn TimingSnapshotLoader>) -> Self {
        Self {
            topology: Arc::new(TransitTopologyIndex::new(network.data())),
            transfers: Arc::new(TransferShortcutIndex::from_network(network.data())),
            network,
            timing,
            bike: None,
        }
    }

    pub fn with_bike_share(
        mut self,
        network: Arc<BikeShareNetwork>,
        availability_source: Arc<dyn BikeAvailabilitySource>,
        street_router: Arc<dyn StreetRouter>,
    ) -> Self {
        self.bike = Some(BikeSegmentPlanner::new(
            network,
            availability_source,
            street_router,
        ));
        self
    }

    pub async fn plan(&self, query: TransitQuery) -> Result<Vec<JourneyVariant>, RoutingError> {
        self.plan_with_stats(query)
            .await
            .map(|(candidates, _)| candidates)
    }

    pub async fn plan_with_stats(
        &self,
        query: TransitQuery,
    ) -> Result<(Vec<JourneyVariant>, TransitPlannerStats), RoutingError> {
        if query.origin_access.is_empty() || query.destination_egress.is_empty() {
            return Err(RoutingError::InvalidQuery);
        }
        let max_legs = query.max_transfers.saturating_add(1);
        let (corridor, static_stats) =
            static_search::generate(&self.network, &self.topology, &self.transfers, &query);
        let mut planner_stats = TransitPlannerStats {
            phase1: static_stats.clone(),
            ..TransitPlannerStats::default()
        };

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
                max_static_search_expansions = query.max_static_search_expansions,
                "static Top-K search hit its safety bound; rejecting the partial corridor set"
            );
            return Err(RoutingError::StaticSearchLimit);
        }
        if corridor.is_empty() {
            return Ok((Vec::new(), planner_stats));
        }

        let positions = corridor.timing_positions().collect::<Vec<_>>();
        let snapshot_limit = usize::try_from(query.max_journey_duration_seconds)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .max(query.max_departures_per_query);
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
                            + Duration::seconds(i64::from(query.max_journey_duration_seconds)),
                    ),
                )
            })
            .collect::<Vec<_>>();
        let timing_positions_requested = timing_queries.len();
        let snapshot_started = Instant::now();
        let has_surface_transfer = corridor.retained().iter().any(|retained| {
            retained
                .steps
                .iter()
                .any(|step| matches!(step, StaticCorridorStep::SurfaceTransfer { .. }))
        });
        let bike_enabled = query.allow_bike_share && self.bike.is_some() && has_surface_transfer;
        let (snapshot, bike_snapshot) = if bike_enabled {
            let bike = self.bike.as_ref().expect("bike planner checked above");
            // TODO: future DepartAt/ArriveBy inventory is undefined; this is a current-count hint,
            // not a forecast or guarantee of future availability.
            let (snapshot, bike_snapshot) = tokio::join!(
                self.timing.load_snapshot(timing_queries),
                bike.availability_source.snapshot()
            );
            let bike_snapshot = match bike_snapshot {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
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
            (self.timing.load_snapshot(timing_queries).await, None)
        };
        tracing::debug!(
            target: "via_server::routing",
            positions_requested = timing_positions_requested,
            snapshot_summary = ?snapshot.trace_summary(),
            relevant_bus_stops = corridor.bus_stops().len(),
            bike_share_enabled = bike_enabled,
            latency_ms = snapshot_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "timing snapshot"
        );
        planner_stats.timing_snapshot_latency_ms = snapshot_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if snapshot.all_positions_failed() {
            return Err(snapshot
                .first_failure()
                .cloned()
                .unwrap_or(crate::domain::transit_timing::DepartureError::TemporarilyUnavailable)
                .into());
        }

        let (bike_realizations, bike_stats) = match (self.bike.as_ref(), bike_snapshot.as_deref()) {
            (Some(bike), Some(snapshot)) => {
                bike.movement_realizations(&corridor, snapshot, OffsetDateTime::now_utc())
                    .await
            }
            _ => (Default::default(), BikePlanningStats::default()),
        };
        let (variants, phase2_stats) = evaluator::evaluate_with_realizations(
            &self.network,
            &corridor,
            &snapshot,
            query,
            &bike_realizations,
        )?;
        planner_stats.phase2_evaluation = phase2_stats;
        planner_stats.bike_share_enabled = bike_enabled;
        planner_stats.relevant_bike_stations = bike_stats.relevant_bike_stations;
        planner_stats.bike_candidate_movements = bike_stats.candidate_movements;
        planner_stats.bike_pickup_stations_considered = bike_stats.pickup_stations_considered;
        planner_stats.bike_dropoff_stations_considered = bike_stats.dropoff_stations_considered;
        planner_stats.bike_station_pairs = bike_stats.station_pairs;
        planner_stats.bike_matrix_requests = bike_stats.matrix_requests;
        planner_stats.bike_realizations_feasible = bike_stats.realizations_feasible;
        planner_stats.multimodal_variants = variants.len();
        tracing::debug!(
            target: "via_server::routing",
            bike_share_enabled = bike_enabled,
            relevant_bike_stations = planner_stats.relevant_bike_stations,
            bike_candidate_movements = planner_stats.bike_candidate_movements,
            bike_pickup_stations_considered = planner_stats.bike_pickup_stations_considered,
            bike_dropoff_stations_considered = planner_stats.bike_dropoff_stations_considered,
            bike_station_pairs = planner_stats.bike_station_pairs,
            bike_matrix_requests = planner_stats.bike_matrix_requests,
            bike_realizations_feasible = planner_stats.bike_realizations_feasible,
            bike_realizations_selected = planner_stats.phase2_evaluation.bike_realizations_selected,
            multimodal_variants = planner_stats.multimodal_variants,
            "Phase 2 corridor evaluation"
        );
        Ok((variants, planner_stats))
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
        api::RequestCoordinate,
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
            RequestCoordinate {
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
            ),
            crate::domain::transit::TransferEdge::new(
                BoardingPointId::new("bus-end-b"),
                BoardingPointId::new("metro-start-b"),
                60,
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
        .with_limits(0, 16, 2_000, 3, 4 * 60 * 60)
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
        query.max_expansions = 1;

        assert!(matches!(
            planner.plan(query).await,
            Err(RoutingError::Phase2EvaluationLimit)
        ));
    }

    #[tokio::test]
    async fn one_bike_snapshot_is_shared_by_multiple_retained_corridors() {
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
            .with_limits(1, 16, 2_000, 3, 4 * 60 * 60)
            .with_bike_share(true);

        let (_, stats) = planner.plan_with_stats(query).await.unwrap();

        assert_eq!(stats.phase1.static_corridors_retained, 2);
        assert_eq!(timing.calls.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn bike_availability_is_not_loaded_without_a_surface_transfer() {
        let timing = Arc::new(RecordingTimingLoader::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let planner = TransitPlanner::new(network(), timing).with_bike_share(
            Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture")),
            Arc::new(CountingBikeAvailabilitySource {
                calls: calls.clone(),
            }),
            Arc::new(FakeStreetRouter::builder().build()),
        );

        let (_, stats) = planner
            .plan_with_stats(query(1).with_bike_share(true))
            .await
            .unwrap();

        assert_eq!(stats.phase1.static_corridors_retained, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
