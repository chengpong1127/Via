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
    reachability::DestinationReachability,
    temporal,
    topology::TransitTopologyIndex,
    transfers::TransferShortcutIndex,
};

#[derive(Clone)]
pub struct TransitPlanner {
    network: Arc<TransitNetwork>,
    topology: Arc<TransitTopologyIndex>,
    transfers: Arc<TransferShortcutIndex>,
    timing: Arc<dyn TimingSnapshotLoader>,
    bike: Option<BikeSegmentPlanner>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransitPlannerStats {
    pub reachable_boarding_opportunities: usize,
    pub reachable_services: usize,
    pub realtime_stops_requested: usize,
    pub temporal_departure_lookups: usize,
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
        let reachability = DestinationReachability::build(
            &self.network,
            &self.topology,
            &self.transfers,
            &query.destination_egress,
            &query.allowed_transit_modes,
            max_legs,
        );
        if reachability.is_empty() {
            return Ok((Vec::new(), TransitPlannerStats::default()));
        }
        let mut planner_stats = TransitPlannerStats {
            reachable_boarding_opportunities: reachability.len(),
            reachable_services: reachability.service_count(),
            realtime_stops_requested: reachability.bus_stops(&self.network).len(),
            ..TransitPlannerStats::default()
        };

        let mut positions = reachability.positions().collect::<Vec<_>>();
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
            relevant_bus_stops = reachability.bus_stops(&self.network).len(),
            bike_share_enabled = bike_enabled,
            latency_ms = snapshot_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "timing snapshot"
        );
        if snapshot.all_positions_failed() {
            return Err(snapshot
                .first_failure()
                .cloned()
                .unwrap_or(crate::domain::transit_timing::DepartureError::TemporarilyUnavailable)
                .into());
        }

        let (candidates, temporal_stats) = temporal::search_with_stats(
            &self.network,
            &self.topology,
            &self.transfers,
            &reachability,
            &snapshot,
            query,
        )?;
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
