use std::{sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::api::GeoCoordinate;
use crate::domain::{
    bikeshare::BikeShareNetwork,
    ids::{BikeStationId, BoardingPointId},
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
    evaluator::WalkingCache,
    evaluator::{self, Phase2EvaluationStats},
    model::{AccessPoint, EgressPoint, JourneyPlanningLimits, PlanningQuery, RoutingError},
    realized::RealizedJourney,
    static_search,
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
};

#[derive(Clone)]
pub struct MultimodalPlanner {
    network: Arc<TransitRuntime>,
    topology: Arc<TransitTopologyIndex>,
    transfers: Arc<TransferShortcutIndex>,
    timing: Arc<dyn TimingSnapshotLoader>,
    bike: BikeShareMovementPlanner,
    street: Arc<dyn StreetRouter>,
}

/// Request-local algorithm accounting for diagnostics and deterministic benchmarks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultimodalPlannerStats {
    pub phase2_evaluation: Phase2EvaluationStats,
    pub phase1: static_search::StaticSearchStats,
}

struct RequestSnapshots {
    timing: crate::domain::transit_timing::TimingSnapshot,
    bike: Option<Arc<crate::domain::bikeshare::BikeAvailabilitySnapshot>>,
    failures: evaluator::EvaluationFailures,
}

impl MultimodalPlanner {
    pub fn new(
        network: Arc<TransitRuntime>,
        timing: Arc<dyn TimingSnapshotLoader>,
        street: Arc<dyn StreetRouter>,
        bike_network: Arc<BikeShareNetwork>,
        bike_availability: Arc<dyn BikeAvailabilitySource>,
    ) -> Self {
        Self {
            topology: Arc::new(TransitTopologyIndex::new(network.network())),
            transfers: Arc::new(TransferShortcutIndex::from_network(network.network())),
            network,
            timing,
            bike: BikeShareMovementPlanner::new(bike_network, bike_availability, street.clone()),
            street,
        }
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
        let mut access_points = matrix
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
        access_points.sort_by(|left, right| {
            left.walking_seconds
                .cmp(&right.walking_seconds)
                .then(left.distance_metres.cmp(&right.distance_metres))
                .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
        });
        Ok(super::access::prune_access_coverage(
            &self.topology,
            access_points,
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
        let mut egress_points = matrix
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
        egress_points.sort_by(|left, right| {
            left.walking_seconds
                .cmp(&right.walking_seconds)
                .then(left.distance_metres.cmp(&right.distance_metres))
                .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
        });
        Ok(super::access::prune_egress_coverage(
            &self.topology,
            egress_points,
            limits.max_transit_egress_stops,
        ))
    }

    pub(crate) fn modes_at_stop(&self, stop_id: &BoardingPointId) -> Vec<TransitMode> {
        super::access::modes_at_stop(&self.network, &self.topology, stop_id)
    }

    pub async fn plan_with_stats(
        &self,
        query: PlanningQuery,
    ) -> Result<(Vec<RealizedJourney>, MultimodalPlannerStats), RoutingError> {
        let mut session = self.prepare(query).await?;
        let mut journeys = session.evaluate().await?;
        journeys.truncate(session.query.limits.max_candidates.max(1));
        session.stats.phase2_evaluation.final_candidates = journeys.len();
        Ok((journeys, session.stats().clone()))
    }

    pub(crate) async fn prepare(
        &self,
        query: PlanningQuery,
    ) -> Result<PlanningSession<'_>, RoutingError> {
        if query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }
        query.validate_access_egress()?;
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
        let mut corridors = retained
            .retained()
            .iter()
            .map(|corridor| EvaluationCorridor::from_transit(&self.network, corridor, &query))
            .collect::<Result<Vec<_>, _>>()?;
        corridors.insert(
            0,
            EvaluationCorridor::direct_walking(query.origin, query.destination),
        );
        let bike_station_ids = if query.allow_bike_share {
            self.bike
                .required_station_ids(corridors.iter().flat_map(|corridor| corridor.anchors()))
        } else {
            Vec::new()
        };
        let loading =
            self.load_request_snapshots(&query, &retained, &static_stats, &bike_station_ids);
        let snapshots = query.deadline.run(loading).await??;
        let walking = WalkingCache::new(self.street.clone());
        let bike_share = snapshots.bike.clone().map(|snapshot| {
            BikeShareSession::new(self.bike.clone(), snapshot, OffsetDateTime::now_utc())
        });
        Ok(PlanningSession {
            network: &self.network,
            query,
            retained,
            corridors,
            snapshots,
            walking,
            bike_share,
            stats: MultimodalPlannerStats {
                phase1: static_stats,
                ..MultimodalPlannerStats::default()
            },
            failures: evaluator::EvaluationFailures::default(),
        })
    }

    fn execute_phase1(
        &self,
        query: &PlanningQuery,
    ) -> Result<
        (
            static_search::RetainedCorridorSet,
            static_search::StaticSearchStats,
        ),
        RoutingError,
    > {
        if query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }
        let (retained_corridors, static_stats) =
            static_search::generate(&self.network, &self.topology, &self.transfers, query);
        if static_stats.deadline_exceeded || query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }

        tracing::debug!(
            target: "via_server::routing",
            states_expanded = static_stats.states_expanded,
            corridors_found = static_stats.unique_corridors_found,
            corridors_retained = static_stats.corridors_retained,
            kth_duration_seconds = ?static_stats.kth_duration_seconds,
            latency_ms = static_stats.search_latency_ms,
            early_terminated = static_stats.top_k_early_terminated,
            "static Top-K corridor search"
        );
        for (index, retained) in retained_corridors.retained().iter().enumerate() {
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
        if static_stats.search_limit_reached {
            tracing::warn!(
                target: "via_server::routing",
                max_static_search_expansions = query.limits.max_phase1_expansions,
                "static Top-K search hit its safety bound; rejecting the partial corridor set"
            );
            return Err(RoutingError::StaticSearchLimit);
        }
        // Count retained work when Phase 1 completes, even if a later stage fails.
        ::metrics::counter!("via_planner_transit_reachable_boardings_total")
            .increment(static_stats.corridor_positions as u64);
        ::metrics::counter!("via_planner_transit_reachable_patterns_total")
            .increment(static_stats.corridor_patterns as u64);
        ::metrics::counter!("via_planner_transit_realtime_stops_requested_total")
            .increment(static_stats.corridor_bus_stops as u64);
        Ok((retained_corridors, static_stats))
    }

    async fn load_request_snapshots(
        &self,
        query: &PlanningQuery,
        retained_corridors: &static_search::RetainedCorridorSet,
        static_stats: &static_search::StaticSearchStats,
        bike_station_ids: &[BikeStationId],
    ) -> Result<RequestSnapshots, RoutingError> {
        let positions = retained_corridors.timing_positions().collect::<Vec<_>>();
        let snapshot_limit = usize::try_from(query.limits.max_journey_duration_seconds)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .max(query.limits.max_departures_per_query);
        let timing_queries = positions
            .into_iter()
            .map(|position| {
                let pattern = self.network.pattern(&position.pattern_id).ok_or_else(|| {
                    RoutingError::Internal("retained timing position is invalid".into())
                })?;
                let stop_id = pattern
                    .stops
                    .get(position.stop_index)
                    .ok_or_else(|| {
                        RoutingError::Internal("retained timing position is invalid".into())
                    })?
                    .clone();
                Ok(DepartureQuery::at_position(
                    position.pattern_id,
                    stop_id,
                    position.stop_index,
                    query.departure_time,
                    snapshot_limit,
                )
                .until(
                    query.departure_time
                        + Duration::seconds(i64::from(query.limits.max_journey_duration_seconds)),
                ))
            })
            .collect::<Result<Vec<_>, RoutingError>>()?;
        let timing_positions_requested = timing_queries.len();
        let snapshot_started = Instant::now();
        let bike_enabled = query.allow_bike_share;
        let mut failures = evaluator::EvaluationFailures::default();
        let timing_future = async {
            if timing_queries.is_empty() {
                crate::domain::transit_timing::TimingSnapshot::default()
            } else {
                self.timing.load_snapshot(timing_queries).await
            }
        };
        let bike_future = async {
            if bike_enabled {
                Some(
                    self.bike
                        .availability_source
                        .snapshot(bike_station_ids)
                        .await,
                )
            } else {
                None
            }
        };
        let (snapshot, bike_result) = tokio::join!(timing_future, bike_future);
        let bike_snapshot = match bike_result {
            None => None,
            Some(Ok(snapshot)) => Some(snapshot),
            Some(Err(error)) => {
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
        if query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }
        tracing::debug!(
            target: "via_server::routing",
            positions_requested = timing_positions_requested,
            snapshot_summary = ?snapshot.trace_summary(),
            relevant_bus_stops = static_stats.corridor_bus_stops,
            bike_share_enabled = bike_enabled,
            latency_ms = snapshot_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "timing snapshot"
        );
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
        })
    }
}

pub(crate) struct PlanningSession<'a> {
    network: &'a TransitRuntime,
    query: PlanningQuery,
    retained: static_search::RetainedCorridorSet,
    corridors: Vec<EvaluationCorridor>,
    snapshots: RequestSnapshots,
    walking: WalkingCache,
    bike_share: Option<BikeShareSession>,
    stats: MultimodalPlannerStats,
    failures: evaluator::EvaluationFailures,
}

impl PlanningSession<'_> {
    pub fn stats(&self) -> &MultimodalPlannerStats {
        &self.stats
    }

    pub fn failures(&self) -> &evaluator::EvaluationFailures {
        &self.failures
    }

    pub async fn evaluate(&mut self) -> Result<Vec<RealizedJourney>, RoutingError> {
        let mut evaluator = evaluator::FixedCorridorEvaluator::new(
            self.network,
            &self.retained,
            &self.snapshots.timing,
            &self.query,
            &mut self.walking,
            self.bike_share.as_mut(),
        );
        let evaluation_result = self
            .query
            .deadline
            .run(evaluator.evaluate(&self.corridors))
            .await??;
        ::metrics::counter!("via_planner_transit_departure_lookups_total")
            .increment(evaluation_result.stats.transit_departure_lookups as u64);
        self.stats.phase2_evaluation = evaluation_result.stats;
        self.failures = evaluation_result.failures;
        self.failures.transient |= self.snapshots.failures.transient;
        self.failures.internal |= self.snapshots.failures.internal;
        Ok(evaluation_result.journeys)
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
        requested: Arc<Mutex<Vec<BikeStationId>>>,
    }

    #[async_trait::async_trait]
    impl BikeAvailabilitySource for CountingBikeAvailabilitySource {
        async fn snapshot(
            &self,
            station_ids: &[BikeStationId],
        ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.requested.lock().unwrap() = station_ids.to_vec();
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

    fn test_planner(
        network: Arc<TransitRuntime>,
        timing: Arc<dyn TimingSnapshotLoader>,
    ) -> MultimodalPlanner {
        MultimodalPlanner::new(
            network,
            timing,
            Arc::new(FakeStreetRouter::builder().build()),
            Arc::new(BikeShareNetwork::new(Vec::new())),
            Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
        )
    }

    fn query(k: usize) -> PlanningQuery {
        PlanningQuery::new(
            Coordinate::try_new(25.0, 121.5).unwrap(),
            Coordinate::try_new(25.0, 121.5).unwrap(),
            super::super::model::PlanningDeadline::after(std::time::Duration::from_secs(60)),
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
    async fn conflicting_duplicates_and_inconsistent_access_times_are_rejected() {
        let loader = Arc::new(RecordingTimingLoader::default());
        let planner = test_planner(network(), loader.clone());
        for modes in [vec![TransitMode::Bus], Vec::new()] {
            for case in 0..4 {
                let mut invalid = query(1).with_allowed_transit_modes(modes.iter().copied());
                match case {
                    0 => {
                        let mut duplicate = invalid.origin_access[0].clone();
                        duplicate.distance_metres += 1;
                        invalid.origin_access.push(duplicate);
                    }
                    1 => {
                        let mut duplicate = invalid.destination_egress[0].clone();
                        duplicate.walking_seconds += 1;
                        invalid.destination_egress.push(duplicate);
                    }
                    2 => invalid.origin_access[0].available_at += Duration::seconds(1),
                    _ => invalid.origin_access[0].departure_at -= Duration::seconds(1),
                }
                assert!(matches!(
                    planner.plan_with_stats(invalid).await,
                    Err(RoutingError::InvalidQuery)
                ));
            }
        }
        assert!(loader.queries.lock().unwrap().is_empty());
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
        let planner = test_planner(network(), loader.clone());

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
        assert_eq!(stats.phase1.corridor_positions, 2);
        assert_eq!(stats.phase1.corridors_retained, 1);
    }

    struct SlowTimingLoader;

    #[async_trait::async_trait]
    impl TimingSnapshotLoader for SlowTimingLoader {
        async fn load_snapshot(&self, _queries: Vec<DepartureQuery>) -> TimingSnapshot {
            tokio::time::sleep(std::time::Duration::from_secs(11)).await;
            TimingSnapshot::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_deadline_awaits_slow_snapshot_loading() {
        let planner = test_planner(network(), Arc::new(SlowTimingLoader));
        let mut query = query(1);
        query.deadline = crate::routing::PlanningDeadline::disabled();
        let started = tokio::time::Instant::now();
        assert!(planner.prepare(query).await.is_ok());
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(11));
    }

    #[tokio::test(start_paused = true)]
    async fn enabled_deadline_cancels_slow_snapshot_loading() {
        let planner = test_planner(network(), Arc::new(SlowTimingLoader));
        let mut query = query(1);
        query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(5));
        let started = tokio::time::Instant::now();
        assert!(matches!(
            planner.prepare(query).await,
            Err(RoutingError::DeadlineExceeded)
        ));
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn expired_deadline_stops_before_loading_timing_snapshot() {
        let loader = Arc::new(RecordingTimingLoader::default());
        let planner = test_planner(network(), loader.clone());
        let mut query = query(1);
        query.deadline = super::super::model::PlanningDeadline::elapsed_for_test();

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
        let planner = test_planner(network(), loader);

        let (candidates, stats) = planner.plan_with_stats(query(2)).await.unwrap();

        assert_eq!(stats.phase1.corridors_retained, 1);
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[1].transit_pattern_sequence(),
            vec![TransitPatternId::new("static-fast")]
        );
        assert_eq!(
            candidates[0].transit_pattern_sequence(),
            vec![TransitPatternId::new("realtime-fast")]
        );
    }

    #[tokio::test]
    async fn operational_metrics_count_completed_stages_once() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = ::metrics::set_default_local_recorder(&recorder);
        let loader = Arc::new(RecordingTimingLoader {
            trips: HashMap::from([(
                TransitPatternId::new("static-fast"),
                vec![effective_trip("static-fast-live", "static-fast", 20)],
            )]),
            ..RecordingTimingLoader::default()
        });
        let origin = Coordinate::try_new(25.0, 121.5).unwrap();
        let destination = Coordinate::try_new(25.001, 121.501).unwrap();
        let planner = test_planner(network(), loader);
        let mut planning_query = query(1);
        planning_query.origin = origin;
        planning_query.destination = destination;
        let mut session = planner.prepare(planning_query).await.unwrap();
        assert!(!session.evaluate().await.unwrap().is_empty());
        let lookups = session.stats().phase2_evaluation.transit_departure_lookups;
        assert!(lookups > 0);
        let rendered = handle.render();
        assert!(rendered.contains(&format!(
            "via_planner_transit_departure_lookups_total {lookups}\n"
        )));
        assert!(rendered.contains("via_planner_transit_reachable_boardings_total 2\n"));
        assert!(rendered.contains("via_planner_transit_reachable_patterns_total 2\n"));
        assert!(rendered.contains("via_planner_transit_realtime_stops_requested_total 1\n"));

        // Completed Phase 1 work remains observable when evaluation hits its bound.
        let mut limited = query(1);
        limited.limits.max_phase2_expansions = 0;
        assert_eq!(
            planner.plan_with_stats(limited).await.unwrap_err(),
            RoutingError::Phase2EvaluationLimit
        );
        let rendered = handle.render();
        assert!(rendered.contains("via_planner_phase2_budget_exhaustions_total 1\n"));
        assert!(rendered.contains("via_planner_transit_reachable_boardings_total 4\n"));
        assert!(rendered.contains(&format!(
            "via_planner_transit_departure_lookups_total {lookups}\n"
        )));
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
        let planner = test_planner(network(), loader);
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
        let requested = Arc::new(Mutex::new(Vec::new()));
        let stop_names = [
            "origin",
            "bus-end-a",
            "metro-start-a",
            "bus-end-b",
            "metro-start-b",
            "destination",
        ];
        let mut stations = Vec::new();
        let mut links = Vec::new();
        for name in stop_names {
            let station_id = BikeStationId::new(format!("linked-{name}"));
            stations.push(crate::domain::bikeshare::BikeStation::new(
                station_id.clone(),
                name,
                GeoCoordinate {
                    latitude: 24.0,
                    longitude: 121.0,
                },
            ));
            links.push(crate::domain::bikeshare::BikeStationAccess::new(
                BoardingPointId::new(name),
                station_id,
                Some(crate::domain::bikeshare::BikeWalkingAccess::new(10, 10)),
                None,
            ));
        }
        stations.push(crate::domain::bikeshare::BikeStation::new(
            BikeStationId::new("origin-nearby"),
            "Origin nearby",
            GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        ));
        stations.push(crate::domain::bikeshare::BikeStation::new(
            BikeStationId::new("destination-nearby"),
            "Destination nearby",
            GeoCoordinate {
                latitude: 25.1,
                longitude: 121.5,
            },
        ));
        let planner = MultimodalPlanner::new(
            multiple_mode_corridors_network(),
            timing.clone(),
            Arc::new(FakeStreetRouter::builder().build()),
            Arc::new(
                BikeShareNetwork::new(stations)
                    .with_access_links(links)
                    .with_revision("fixture"),
            ),
            Arc::new(CountingBikeAvailabilitySource {
                calls: calls.clone(),
                requested: requested.clone(),
            }),
        );
        let mut bike_query = query(2)
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

        bike_query.destination = Coordinate::try_new(25.1, 121.5).unwrap();
        let (_, stats) = planner.plan_with_stats(bike_query).await.unwrap();

        assert_eq!(stats.phase1.corridors_retained, 2);
        let loaded = requested.lock().unwrap().clone();
        assert_eq!(loaded.len(), 8);
        for name in stop_names {
            assert!(loaded.contains(&BikeStationId::new(format!("linked-{name}"))));
        }
        assert!(loaded.contains(&BikeStationId::new("origin-nearby")));
        assert!(loaded.contains(&BikeStationId::new("destination-nearby")));
        assert_eq!(timing.calls.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        planner.plan_with_stats(query(1)).await.unwrap();
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
        let planner = MultimodalPlanner::new(
            network(),
            timing.clone(),
            street,
            Arc::new(BikeShareNetwork::new(Vec::new())),
            Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
        );
        let query = PlanningQuery::new(
            origin,
            destination,
            super::super::model::PlanningDeadline::after(std::time::Duration::from_secs(60)),
            Vec::new(),
            Vec::new(),
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes([]);
        let (journeys, stats) = planner.plan_with_stats(query).await.unwrap();
        assert_eq!(journeys.len(), 1);
        assert_eq!(stats.phase1.states_expanded, 0);
        assert_eq!(timing.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn direct_walking_is_evaluated_once_without_transit() {
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
        let planner = MultimodalPlanner::new(
            network(),
            timing,
            street,
            Arc::new(BikeShareNetwork::new(Vec::new())),
            Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
        );
        let query = PlanningQuery::new(
            origin,
            destination,
            super::super::model::PlanningDeadline::after(std::time::Duration::from_secs(60)),
            Vec::new(),
            Vec::new(),
            datetime!(2026-09-21 10:00 UTC),
        )
        .with_allowed_transit_modes([]);
        let mut session = planner.prepare(query).await.unwrap();

        let primary = session.evaluate().await.unwrap();

        assert_eq!(primary.len(), 1);
        assert_eq!(
            session.stats().phase2_evaluation.corridor_states_evaluated,
            1
        );
    }
}
