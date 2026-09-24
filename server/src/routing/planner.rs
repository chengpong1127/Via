//! Canonical two-phase transit and BikeShare planner.

use std::{sync::Arc, time::Instant};

use time::Duration;

use crate::domain::{
    bikeshare::BikeShareNetwork,
    transit::TransitNetwork,
    transit_timing::{DepartureQuery, TimingSnapshotLoader},
};
use crate::{providers::BikeAvailabilitySource, street::StreetRouter};

use super::{
    bike::{BikePlanningStats, BikeSegmentPlanner},
    model::{RoutingError, TransitQuery},
    path::{JourneySkeleton, JourneyVariant},
    static_search::{self, StaticTravelTimeIndex},
    temporal,
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
};

#[derive(Clone)]
pub struct TransitPlanner {
    network: Arc<TransitNetwork>,
    topology: Arc<TransitTopologyIndex>,
    transfers: Arc<TransferShortcutIndex>,
    static_travel_times: Arc<StaticTravelTimeIndex>,
    timing: Arc<dyn TimingSnapshotLoader>,
    bike: Option<BikeSegmentPlanner>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransitPlannerStats {
    pub reachable_boarding_opportunities: usize,
    pub reachable_services: usize,
    pub realtime_stops_requested: usize,
    pub temporal_departure_lookups: usize,
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
    pub timing_snapshot_latency_ms: u64,
    pub temporal_search_latency_ms: u64,
    pub kth_static_duration_seconds: Option<u32>,
    pub exact_schedule_cost: usize,
    pub derived_bus_cost: usize,
    pub estimated_cost: usize,
    pub mixed_cost: usize,
    pub static_search_limit_reached: bool,
    pub bike_share_enabled: bool,
    pub relevant_bike_stations: usize,
    pub bike_candidate_segments: usize,
    pub bike_station_pairs: usize,
    pub bike_matrix_requests: usize,
    pub bike_shortcuts_accepted: usize,
    pub multimodal_variants: usize,
}

impl TransitPlanner {
    pub fn new(network: Arc<TransitNetwork>, timing: Arc<dyn TimingSnapshotLoader>) -> Self {
        Self {
            topology: Arc::new(TransitTopologyIndex::new(&network)),
            transfers: Arc::new(TransferShortcutIndex::from_network(&network)),
            static_travel_times: Arc::new(StaticTravelTimeIndex::new(&network)),
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
        let (corridor, static_stats) = static_search::generate(
            &self.network,
            &self.topology,
            &self.transfers,
            &self.static_travel_times,
            &query,
        );
        let mut planner_stats = TransitPlannerStats {
            reachable_boarding_opportunities: corridor.len(),
            reachable_services: corridor.service_count(),
            realtime_stops_requested: corridor.bus_stops().len(),
            static_search_states_expanded: static_stats.static_search_states_expanded,
            static_service_scans: static_stats.static_service_scans,
            static_positions_scanned: static_stats.static_positions_scanned,
            static_complete_candidates_found: static_stats.static_complete_candidates_found,
            static_unique_corridors_found: static_stats.static_unique_corridors_found,
            static_corridors_retained: static_stats.static_corridors_retained,
            static_corridor_services: static_stats.static_corridor_services,
            static_corridor_positions: static_stats.static_corridor_positions,
            static_corridor_bus_stops: static_stats.static_corridor_bus_stops,
            static_search_latency_ms: static_stats.static_search_latency_ms,
            kth_static_duration_seconds: static_stats.kth_static_duration_seconds,
            exact_schedule_cost: static_stats.exact_schedule_cost,
            derived_bus_cost: static_stats.derived_bus_cost,
            estimated_cost: static_stats.estimated_cost,
            mixed_cost: static_stats.mixed_cost,
            static_search_limit_reached: static_stats.static_search_limit_reached,
            ..TransitPlannerStats::default()
        };

        tracing::debug!(
            target: "via_server::routing",
            static_search_states_expanded = static_stats.static_search_states_expanded,
            static_service_scans = static_stats.static_service_scans,
            static_positions_scanned = static_stats.static_positions_scanned,
            static_complete_candidates_found = static_stats.static_complete_candidates_found,
            static_unique_corridors_found = static_stats.static_unique_corridors_found,
            static_corridors_retained = static_stats.static_corridors_retained,
            static_corridor_services = static_stats.static_corridor_services,
            static_corridor_positions = static_stats.static_corridor_positions,
            static_corridor_bus_stops = static_stats.static_corridor_bus_stops,
            static_search_latency_ms = static_stats.static_search_latency_ms,
            kth_static_duration_seconds = ?static_stats.kth_static_duration_seconds,
            exact_schedule_cost = static_stats.exact_schedule_cost,
            derived_bus_cost = static_stats.derived_bus_cost,
            estimated_cost = static_stats.estimated_cost,
            mixed_cost = static_stats.mixed_cost,
            static_search_limit_reached = static_stats.static_search_limit_reached,
            max_legs,
            "static Top-K corridor search"
        );
        for (index, retained) in corridor.retained().iter().enumerate() {
            tracing::debug!(
                target: "via_server::routing",
                corridor_rank = index + 1,
                theoretical_duration_seconds = retained.theoretical_duration_seconds,
                service_sequence = ?retained.service_sequence(),
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

        let mut positions = corridor.positions().collect::<Vec<_>>();
        positions.sort_by(|left, right| {
            left.service_id
                .as_str()
                .cmp(right.service_id.as_str())
                .then(left.stop_index.cmp(&right.stop_index))
        });
        let snapshot_limit = usize::try_from(query.max_journey_duration_seconds)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .max(query.max_departures_per_query);
        let timing_queries = positions
            .into_iter()
            .filter_map(|position| {
                let service = self.network.service(&position.service_id)?;
                let stop_id = service.pattern.get(position.stop_index)?.clone();
                Some(
                    DepartureQuery::at_position(
                        position.service_id,
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
        let bike_enabled = query.allow_bike_share && self.bike.is_some();
        let max_variants = query.max_candidates;
        let (snapshot, bike_snapshot) = if bike_enabled {
            let bike = self.bike.as_ref().expect("bike planner checked above");
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

        let temporal_started = Instant::now();
        let (candidates, temporal_stats) = temporal::search_with_stats(
            &self.network,
            &self.topology,
            &self.transfers,
            &corridor,
            &snapshot,
            query,
        )?;
        planner_stats.temporal_search_latency_ms = temporal_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        planner_stats.temporal_departure_lookups = temporal_stats.departure_lookups;
        planner_stats.bike_share_enabled = bike_enabled;

        let skeletons = candidates
            .into_iter()
            .filter_map(|candidate| match JourneySkeleton::from_candidate(candidate) {
                Ok(skeleton) => Some(skeleton),
                Err(error) => {
                    tracing::error!(error = %error, "journey candidate could not be projected to a path");
                    None
                }
            })
            .collect::<Vec<_>>();
        let (variants, bike_stats) = match (self.bike.as_ref(), bike_snapshot.as_deref()) {
            (Some(bike), Some(snapshot)) => {
                bike.choose_variants(skeletons, snapshot, max_variants)
                    .await
            }
            _ => (
                skeletons
                    .into_iter()
                    .map(JourneyVariant::baseline)
                    .collect(),
                BikePlanningStats::default(),
            ),
        };
        planner_stats.relevant_bike_stations = bike_stats.relevant_bike_stations;
        planner_stats.bike_candidate_segments = bike_stats.candidate_segments;
        planner_stats.bike_station_pairs = bike_stats.station_pairs;
        planner_stats.bike_matrix_requests = bike_stats.matrix_requests;
        planner_stats.bike_shortcuts_accepted = bike_stats.shortcuts_accepted;
        planner_stats.multimodal_variants = variants.len();
        tracing::debug!(
            target: "via_server::routing",
            bike_share_enabled = bike_enabled,
            relevant_bike_stations = planner_stats.relevant_bike_stations,
            bike_candidate_segments = planner_stats.bike_candidate_segments,
            bike_station_pairs = planner_stats.bike_station_pairs,
            bike_matrix_requests = planner_stats.bike_matrix_requests,
            bike_shortcuts_accepted = planner_stats.bike_shortcuts_accepted,
            multimodal_variants = planner_stats.multimodal_variants,
            "temporal multimodal planning"
        );
        Ok((variants, planner_stats))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use time::{Time, Weekday, macros::datetime};

    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::{
            ids::{BoardingPointId, EffectiveTripId, ScheduledTripId, TransitServiceId},
            transit::{ScheduledStopTime, ScheduledTrip, TransitMode, TransitService, TransitStop},
            transit_timing::{
                EffectiveStopTime, EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot,
            },
        },
        routing::{AccessPoint, EgressPoint},
    };

    #[derive(Default)]
    struct RecordingTimingLoader {
        queries: Mutex<Vec<DepartureQuery>>,
        trips: HashMap<TransitServiceId, Vec<EffectiveTrip>>,
    }

    #[async_trait::async_trait]
    impl TimingSnapshotLoader for RecordingTimingLoader {
        async fn load_snapshot(&self, queries: Vec<DepartureQuery>) -> TimingSnapshot {
            *self.queries.lock().unwrap() = queries.clone();
            let mut snapshot = TimingSnapshot::default();
            for query in queries {
                snapshot.insert(
                    query.service_id.clone(),
                    query.board_index.unwrap(),
                    self.trips
                        .get(&query.service_id)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            snapshot
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

    fn service(value: &str) -> TransitService {
        TransitService::new(
            TransitServiceId::new(value),
            TransitMode::Bus,
            value,
            "destination",
            vec![
                BoardingPointId::new("origin"),
                BoardingPointId::new("destination"),
            ],
        )
    }

    fn scheduled_trip(value: &str, service_id: &str, duration: u32) -> ScheduledTrip {
        ScheduledTrip::weekdays(
            ScheduledTripId::new(value),
            TransitServiceId::new(service_id),
            vec![Weekday::Monday],
            vec![
                ScheduledStopTime::new(0, 0),
                ScheduledStopTime::new(duration, duration),
            ],
        )
    }

    fn effective_trip(value: &str, service_id: &str, arrival_minute: i64) -> EffectiveTrip {
        EffectiveTrip {
            id: EffectiveTripId::new(value),
            service_id: TransitServiceId::new(service_id),
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

    fn network() -> Arc<TransitNetwork> {
        Arc::new(TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("origin"), stop("destination")],
            vec![service("static-fast"), service("realtime-fast")],
            vec![
                scheduled_trip("static-fast-trip", "static-fast", 100),
                scheduled_trip("realtime-fast-trip", "realtime-fast", 200),
            ],
        ))
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
    async fn timing_queries_include_only_positions_in_retained_corridors() {
        let loader = Arc::new(RecordingTimingLoader {
            trips: HashMap::from([
                (
                    TransitServiceId::new("static-fast"),
                    vec![effective_trip("static-fast-live", "static-fast", 20)],
                ),
                (
                    TransitServiceId::new("realtime-fast"),
                    vec![effective_trip("realtime-fast-live", "realtime-fast", 10)],
                ),
            ]),
            ..RecordingTimingLoader::default()
        });
        let planner = TransitPlanner::new(network(), loader.clone());

        let (_, stats) = planner.plan_with_stats(query(1)).await.unwrap();

        let queries = loader.queries.lock().unwrap();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].service_id, TransitServiceId::new("static-fast"));
        assert_eq!(stats.static_corridor_positions, 1);
    }

    #[tokio::test]
    async fn temporal_search_chooses_actual_best_journey_within_retained_corridors() {
        let loader = Arc::new(RecordingTimingLoader {
            trips: HashMap::from([
                (
                    TransitServiceId::new("static-fast"),
                    vec![effective_trip("static-fast-live", "static-fast", 20)],
                ),
                (
                    TransitServiceId::new("realtime-fast"),
                    vec![effective_trip("realtime-fast-live", "realtime-fast", 10)],
                ),
            ]),
            ..RecordingTimingLoader::default()
        });
        let planner = TransitPlanner::new(network(), loader);

        let (candidates, stats) = planner.plan_with_stats(query(2)).await.unwrap();

        assert_eq!(stats.static_corridors_retained, 2);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].transit_service_sequence(),
            vec![TransitServiceId::new("realtime-fast")]
        );
    }
}
