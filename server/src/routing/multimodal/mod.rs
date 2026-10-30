mod discovery;
#[cfg(test)]
mod tests;

use std::{sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};
use tracing::Instrument;

use crate::{
    domain::{
        bikeshare::{BikeAvailabilitySnapshot, BikeAvailabilitySource, BikeShareNetwork},
        ids::BoardingPointId,
        journey::PlanningError,
        location::GeoCoordinate,
        transit::TransitMode,
        transit_runtime::TransitRuntime,
        transit_timing::{DepartureQuery, TimingSnapshot, TimingSnapshotLoader},
    },
    street::{
        Coordinate, MatrixCell, MatrixOrientation, MatrixQuery, RoutingConstraints, StreetRouter,
        StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    access,
    bike::{BikeMovementPolicy, BikeMovementSource, BikeShareMovementPlanner, BikeShareSession},
    cache::MatrixCache,
    corridor::EvaluationCorridor,
    evaluator::{CorridorEvaluator, Phase2EvaluationStats},
    model::{
        AccessPoint, EgressPoint, FailureSummary, JourneyPlanningContext, JourneyPlanningLimits,
        PlanningQuery, RoutingError,
    },
    realized::RealizedJourney,
    reconstruct::to_street_coordinate,
    static_search::{self, RetainedCorridorSet, StaticSearchStats},
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
    pub phase1: StaticSearchStats,
    pub phase2: Phase2EvaluationStats,
}

pub(crate) struct PlannedJourneys {
    pub journeys: Vec<RealizedJourney>,
    pub stats: MultimodalPlannerStats,
    pub failures: FailureSummary,
}

struct RequestSnapshots {
    timing: TimingSnapshot,
    bike: Option<Arc<BikeAvailabilitySnapshot>>,
}

impl MultimodalPlanner {
    pub fn new(
        network: Arc<TransitRuntime>,
        timing: Arc<dyn TimingSnapshotLoader>,
        street: Arc<dyn StreetRouter>,
        bike_network: Arc<BikeShareNetwork>,
        bike_availability: Arc<dyn BikeAvailabilitySource>,
        bike: BikeMovementPolicy,
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
        let mut planned = self.plan(query).await?;
        planned.journeys.truncate(limit);
        planned.stats.phase2.final_candidates = planned.journeys.len();
        Ok((planned.journeys, planned.stats))
    }

    pub(crate) async fn plan(&self, query: PlanningQuery) -> Result<PlannedJourneys, RoutingError> {
        if query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }
        query.validate_access_egress()?;
        let (retained, phase1) = if query.allowed_transit_modes.is_empty()
            || query.origin_access.is_empty()
            || query.destination_egress.is_empty()
        {
            (RetainedCorridorSet::default(), StaticSearchStats::default())
        } else {
            self.search_static_corridors(&query)?
        };
        let mut corridors = vec![EvaluationCorridor::direct_walking(
            query.origin,
            query.destination,
        )];
        for corridor in retained.retained() {
            corridors.push(EvaluationCorridor::from_transit(
                &self.network,
                corridor,
                &query,
            )?);
        }
        let (snapshots, mut failures) = query
            .deadline
            .run(self.load_snapshots(&query, &retained, &phase1, &corridors))
            .await??;
        let street = Arc::new(MatrixCache::new(self.street.clone()));
        let bike = snapshots.bike.map(|snapshot| {
            BikeShareSession::new(
                self.bike.clone(),
                snapshot,
                OffsetDateTime::now_utc(),
                street.clone(),
            )
        });
        let evaluation = CorridorEvaluator {
            network: &self.network,
            retained: &retained,
            timing: &snapshots.timing,
            query: &query,
            street: street.as_ref(),
            bike: bike.as_ref().map(|bike| bike as &dyn BikeMovementSource),
        }
        .evaluate(&corridors)
        .instrument(tracing::info_span!(
            target: "via_profile",
            "Phase 2",
            corridor_count = corridors.len()
        ))
        .await?;
        let mut phase2 = evaluation.stats;
        if let Some(bike) = &bike {
            phase2.bike_requests = bike.requests();
            phase2.bike_cache_hits = bike.hits();
        }
        ::metrics::counter!("via_planner_transit_departure_lookups_total")
            .increment(phase2.transit_departure_lookups as u64);
        tracing::debug!(target: "via_server::routing", stats = ?phase2, "Phase 2 corridor evaluation");
        failures.merge(evaluation.failures);
        Ok(PlannedJourneys {
            journeys: evaluation.journeys,
            stats: MultimodalPlannerStats { phase1, phase2 },
            failures,
        })
    }

    #[tracing::instrument(target = "via_profile", name = "Phase 1", skip_all, fields(states_expanded = tracing::field::Empty, corridor_count = tracing::field::Empty))]
    fn search_static_corridors(
        &self,
        query: &PlanningQuery,
    ) -> Result<(RetainedCorridorSet, StaticSearchStats), RoutingError> {
        let (retained, stats) =
            static_search::generate(&self.network, &self.topology, &self.transfers, query);
        tracing::Span::current()
            .record("states_expanded", stats.states_expanded)
            .record("corridor_count", stats.corridors_retained);
        if stats.deadline_exceeded || query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }
        tracing::debug!(
            target: "via_server::routing",
            states_expanded = stats.states_expanded,
            corridors_found = stats.unique_corridors_found,
            corridors_retained = stats.corridors_retained,
            kth_duration_seconds = ?stats.kth_duration_seconds,
            latency_ms = stats.search_latency_ms,
            early_terminated = stats.top_k_early_terminated,
            "static Top-K corridor search"
        );
        for (index, corridor) in retained.retained().iter().enumerate() {
            tracing::debug!(
                target: "via_server::routing",
                corridor_rank = index + 1,
                theoretical_duration_seconds = corridor.theoretical_duration_seconds,
                pattern_sequence = ?corridor.pattern_sequence(),
                structural_steps = ?corridor.steps,
                cost_source = corridor.cost_source.as_str(),
                "retained static corridor"
            );
        }
        if stats.search_limit_reached {
            tracing::warn!(
                target: "via_server::routing",
                max_static_search_expansions = query.limits.max_phase1_expansions,
                "static Top-K search hit its safety bound; rejecting the partial corridor set"
            );
            return Err(RoutingError::StaticSearchLimit);
        }
        ::metrics::counter!("via_planner_transit_reachable_boardings_total")
            .increment(stats.corridor_positions as u64);
        ::metrics::counter!("via_planner_transit_reachable_patterns_total")
            .increment(stats.corridor_patterns as u64);
        ::metrics::counter!("via_planner_transit_realtime_stops_requested_total")
            .increment(stats.corridor_bus_stops as u64);
        Ok((retained, stats))
    }

    #[tracing::instrument(target = "via_profile", name = "Snapshot Loading", skip_all, fields(corridor_count = retained.retained().len()))]
    async fn load_snapshots(
        &self,
        query: &PlanningQuery,
        retained: &RetainedCorridorSet,
        phase1: &StaticSearchStats,
        corridors: &[EvaluationCorridor],
    ) -> Result<(RequestSnapshots, FailureSummary), RoutingError> {
        let window_end = query.departure_time
            + Duration::seconds(i64::from(query.limits.max_journey_duration_seconds));
        let departures_per_position = usize::try_from(query.limits.max_journey_duration_seconds)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .max(query.limits.max_departures_per_query);
        let timing_queries = retained
            .timing_positions()
            .map(|position| {
                let stop_id = self
                    .network
                    .pattern(&position.pattern_id)
                    .and_then(|pattern| pattern.stops.get(position.stop_index))
                    .ok_or_else(|| {
                        RoutingError::Internal("retained timing position is invalid".into())
                    })?
                    .clone();
                Ok(DepartureQuery::at_position(
                    position.pattern_id,
                    stop_id,
                    position.stop_index,
                    query.departure_time,
                    departures_per_position,
                )
                .until(window_end))
            })
            .collect::<Result<Vec<_>, RoutingError>>()?;
        let bike_station_ids = if query.allow_bike_share {
            self.bike
                .required_station_ids(corridors.iter().flat_map(EvaluationCorridor::anchors))
        } else {
            Vec::new()
        };
        let positions_requested = timing_queries.len();
        let started = Instant::now();
        let timing = async {
            if timing_queries.is_empty() {
                TimingSnapshot::default()
            } else {
                self.timing.load_snapshot(timing_queries).await
            }
        };
        let bike = async {
            if query.allow_bike_share {
                Some(self.bike_availability.snapshot(&bike_station_ids).await)
            } else {
                None
            }
        };
        let (timing, bike) = tokio::join!(
            timing.instrument(tracing::info_span!(target: "via_profile", "TDX Timing")),
            bike.instrument(tracing::info_span!(
                target: "via_profile",
                "BikeShare Availability",
                enabled = query.allow_bike_share
            ))
        );
        let mut failures = FailureSummary::default();
        let bike = match bike {
            None => None,
            Some(Ok(snapshot)) => Some(snapshot),
            Some(Err(error)) => {
                failures.record_bike_availability(&error);
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
            positions_requested,
            snapshot_summary = ?timing.trace_summary(),
            relevant_bus_stops = phase1.corridor_bus_stops,
            bike_share_enabled = query.allow_bike_share,
            bike_station_count = bike_station_ids.len(),
            latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "timing snapshot"
        );
        if timing.all_positions_failed()
            && let Some(error) = timing.first_failure()
        {
            failures.record_departure(error);
        }
        Ok((RequestSnapshots { timing, bike }, failures))
    }
}
