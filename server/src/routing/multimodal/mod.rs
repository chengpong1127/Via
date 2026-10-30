mod access;
mod session;
mod snapshots;
#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::domain::bikeshare::BikeAvailabilityError;
use session::PlanningSession;
use std::{sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::domain::bikeshare::BikeAvailabilitySource;
use crate::domain::location::GeoCoordinate;
use crate::domain::{
    bikeshare::BikeShareNetwork,
    ids::{BikeStationId, BoardingPointId},
    transit::TransitMode,
    transit_runtime::TransitRuntime,
    transit_timing::{DepartureQuery, TimingSnapshotLoader},
};
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
    bike_availability: Arc<dyn BikeAvailabilitySource>,
    street: Arc<dyn StreetRouter>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultimodalPlannerStats {
    pub phase2_evaluation: Phase2EvaluationStats,
    pub phase1: static_search::StaticSearchStats,
}

struct RequestSnapshots {
    timing: crate::domain::transit_timing::TimingSnapshot,
    bike: Option<Arc<crate::domain::bikeshare::BikeAvailabilitySnapshot>>,
    failures: crate::routing::failures::FailureSummary,
}

impl MultimodalPlanner {
    pub fn new(
        network: Arc<TransitRuntime>,
        timing: Arc<dyn TimingSnapshotLoader>,
        street: Arc<dyn StreetRouter>,
        bike_network: Arc<BikeShareNetwork>,
        bike_availability: Arc<dyn BikeAvailabilitySource>,
        bike: crate::routing::BikeMovementPolicy,
    ) -> Self {
        Self {
            topology: Arc::new(TransitTopologyIndex::new(network.network())),
            transfers: Arc::new(TransferShortcutIndex::from_network(network.network())),
            network,
            timing,
            bike: BikeShareMovementPlanner::new(bike_network, bike),
            street,
            bike_availability,
        }
    }

    pub async fn plan_with_stats(
        &self,
        query: PlanningQuery,
    ) -> Result<(Vec<RealizedJourney>, MultimodalPlannerStats), RoutingError> {
        let limit = query.limits.max_candidates.max(1);
        let report = self.prepare(query).await?.evaluate().await;
        let mut journeys = report.outcome?;
        journeys.truncate(limit);
        let mut stats = report.stats;
        stats.phase2_evaluation.final_candidates = journeys.len();
        Ok((journeys, stats))
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
        let matrix = Arc::new(super::cache::SharedMatrixResolver::new(self.street.clone()));
        let walking = WalkingCache::with_resolver(matrix.clone());
        let bike_share = snapshots.bike.clone().map(|snapshot| {
            BikeShareSession::new(
                self.bike.clone(),
                snapshot,
                OffsetDateTime::now_utc(),
                matrix,
            )
        });
        Ok(PlanningSession {
            network: &self.network,
            query,
            retained,
            corridors,
            snapshots,
            walking,
            bike_share,
            static_stats,
        })
    }

    #[tracing::instrument(target = "via_profile", name = "Phase 1", skip_all, fields(states_expanded = tracing::field::Empty, corridor_count = tracing::field::Empty))]
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
        tracing::Span::current()
            .record("states_expanded", static_stats.states_expanded)
            .record("corridor_count", static_stats.corridors_retained);
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
        ::metrics::counter!("via_planner_transit_reachable_boardings_total")
            .increment(static_stats.corridor_positions as u64);
        ::metrics::counter!("via_planner_transit_reachable_patterns_total")
            .increment(static_stats.corridor_patterns as u64);
        ::metrics::counter!("via_planner_transit_realtime_stops_requested_total")
            .increment(static_stats.corridor_bus_stops as u64);
        Ok((retained_corridors, static_stats))
    }
}
