//! Canonical three-step transit planner.

use std::{sync::Arc, time::Instant};

use time::Duration;

use crate::domain::{
    transit::TransitNetwork,
    transit_timing::{DepartureQuery, TimingSnapshotLoader},
};

use super::{
    model::{JourneyCandidate, RoutingError, TransitQuery},
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
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransitPlannerStats {
    pub reachable_boarding_opportunities: usize,
    pub reachable_services: usize,
    pub realtime_stops_requested: usize,
    pub temporal_departure_lookups: usize,
}

impl TransitPlanner {
    pub fn new(network: Arc<TransitNetwork>, timing: Arc<dyn TimingSnapshotLoader>) -> Self {
        Self {
            topology: Arc::new(TransitTopologyIndex::new(&network)),
            transfers: Arc::new(TransferShortcutIndex::from_network(&network)),
            network,
            timing,
        }
    }

    pub async fn plan(&self, query: TransitQuery) -> Result<Vec<JourneyCandidate>, RoutingError> {
        self.plan_with_stats(query)
            .await
            .map(|(candidates, _)| candidates)
    }

    pub async fn plan_with_stats(
        &self,
        query: TransitQuery,
    ) -> Result<(Vec<JourneyCandidate>, TransitPlannerStats), RoutingError> {
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
        let snapshot = self.timing.load_snapshot(timing_queries).await;
        tracing::debug!(
            target: "via_server::routing",
            relevant_bus_stops = reachability.bus_stops(&self.network).len(),
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
        Ok((candidates, planner_stats))
    }
}
