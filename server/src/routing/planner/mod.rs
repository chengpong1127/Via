#[cfg(test)]
mod tests;

use std::{collections::HashSet, sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};
use tracing::Instrument;

use crate::{
    domain::{
        bikeshare::{BikeAvailabilitySnapshot, BikeAvailabilitySource, BikeShareNetwork},
        journey::PlanningError,
        transit::network::TransitMode,
        transit::runtime::{PatternPosition, TransitRuntime},
        transit::timing::{DepartureQuery, TimingSnapshot, TimingSnapshotLoader},
    },
    street::StreetRouter,
};

use super::{
    access,
    cache::{MatrixCache, RouteCache},
    corridors::{self, Corridor, CorridorSearchStats},
    evaluation::{
        CorridorEvaluationStats, CorridorEvaluator,
        bike::{
            BikeAvailabilityLimits, BikeMovementPolicy, BikeMovementSource,
            BikeShareMovementPlanner, BikeShareSession,
        },
    },
    journey::RealizedJourney,
    model::{
        FailureSummary, JourneyPlanningContext, JourneyPlanningLimits, PlanningQuery, RoutingError,
    },
    reconstruct::to_street_coordinate,
};

#[derive(Clone)]
pub struct MultimodalPlanner {
    transit: Arc<TransitRuntime>,
    timing_loader: Arc<dyn TimingSnapshotLoader>,
    bike_planner: BikeShareMovementPlanner,
    bike_availability: Arc<dyn BikeAvailabilitySource>,
    street_router: Arc<dyn StreetRouter>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultimodalPlannerStats {
    pub search: CorridorSearchStats,
    pub evaluation: CorridorEvaluationStats,
}

pub(crate) struct PlannedJourneys {
    pub journeys: Vec<RealizedJourney>,
    pub stats: MultimodalPlannerStats,
    pub failures: FailureSummary,
}

struct RequestSnapshots {
    timing: TimingSnapshot,
    bike_availability: Option<Arc<BikeAvailabilitySnapshot>>,
}

impl MultimodalPlanner {
    pub fn new(
        transit: Arc<TransitRuntime>,
        timing_loader: Arc<dyn TimingSnapshotLoader>,
        street_router: Arc<dyn StreetRouter>,
        bike_share: Arc<BikeShareNetwork>,
        bike_availability: Arc<dyn BikeAvailabilitySource>,
        bike_policy: BikeMovementPolicy,
        bike_availability_limits: BikeAvailabilityLimits,
    ) -> Self {
        Self {
            transit,
            timing_loader,
            bike_planner: BikeShareMovementPlanner::new(
                bike_share,
                bike_policy,
                bike_availability_limits,
            ),
            street_router,
            bike_availability,
        }
    }

    pub(crate) async fn discover_query(
        &self,
        context: &JourneyPlanningContext<'_>,
        limits: &JourneyPlanningLimits,
    ) -> Result<(PlanningQuery, FailureSummary), PlanningError> {
        let request = context.request;
        let modes = request.transit_modes.clone();
        let origin = to_street_coordinate(request.origin.coordinate)?;
        let destination = to_street_coordinate(request.destination.coordinate)?;
        let mut failures = FailureSummary::default();
        let (access, egress) = access::discover(
            &self.transit,
            self.street_router.as_ref(),
            context,
            limits,
            &modes,
            &mut failures,
        )
        .instrument(tracing::info_span!(target: "via_profile", "Candidate Discovery"))
        .await?;
        let query = PlanningQuery::new(
            origin,
            destination,
            context.deadline,
            access,
            egress,
            context.search_start,
            limits.planner,
        )
        .with_allowed_transit_modes(modes)
        .with_bike_share(request.allow_bike_share);
        Ok((query, failures))
    }

    pub async fn plan_with_stats(
        &self,
        query: PlanningQuery,
    ) -> Result<(Vec<RealizedJourney>, MultimodalPlannerStats), RoutingError> {
        let limit = query.limits.max_candidates.max(1);
        let route_cache = Arc::new(RouteCache::new(self.street_router.clone()));
        let mut planned = self.plan(query, route_cache).await?;
        planned.journeys.truncate(limit);
        planned.stats.evaluation.final_candidates = planned.journeys.len();
        Ok((planned.journeys, planned.stats))
    }

    pub(crate) async fn plan(
        &self,
        query: PlanningQuery,
        route_cache: Arc<RouteCache>,
    ) -> Result<PlannedJourneys, RoutingError> {
        if query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }
        query.validate_access_egress()?;
        let (transit_corridors, search) = if query.allowed_transit_modes.is_empty()
            || query.origin_access.is_empty()
            || query.destination_egress.is_empty()
        {
            (Vec::new(), CorridorSearchStats::default())
        } else {
            self.search_static_corridors(&query)?
        };
        let mut corridors = vec![Corridor::direct_walking(query.origin, query.destination)];
        corridors.extend(transit_corridors);
        let (snapshots, mut failures) = query
            .deadline
            .run(self.load_snapshots(&query, &corridors))
            .await??;
        let matrix_cache = Arc::new(MatrixCache::new(route_cache));
        let bike_session = snapshots.bike_availability.map(|availability| {
            BikeShareSession::new(
                self.bike_planner.clone(),
                availability,
                OffsetDateTime::now_utc(),
                matrix_cache.clone(),
            )
        });
        let evaluation = CorridorEvaluator {
            transit: &self.transit,
            timing: &snapshots.timing,
            query: &query,
            matrix_cache: matrix_cache.as_ref(),
            bike_movements: bike_session
                .as_ref()
                .map(|session| session as &dyn BikeMovementSource),
        }
        .evaluate(&corridors)
        .instrument(tracing::info_span!(
            target: "via_profile",
            "Corridor Evaluation",
            corridor_count = corridors.len()
        ))
        .await?;
        let mut stats = evaluation.stats;
        if let Some(session) = &bike_session {
            stats.bike_requests = session.requests();
            stats.bike_cache_hits = session.hits();
        }
        ::metrics::counter!("via_planner_transit_departure_lookups_total")
            .increment(stats.transit_departure_lookups as u64);
        tracing::debug!(target: "via_server::routing", stats = ?stats, "corridor evaluation");
        failures.merge(evaluation.failures);
        Ok(PlannedJourneys {
            journeys: evaluation.journeys,
            stats: MultimodalPlannerStats {
                search,
                evaluation: stats,
            },
            failures,
        })
    }

    #[tracing::instrument(target = "via_profile", name = "Corridor Search", skip_all, fields(states_expanded = tracing::field::Empty, corridor_count = tracing::field::Empty))]
    fn search_static_corridors(
        &self,
        query: &PlanningQuery,
    ) -> Result<(Vec<Corridor>, CorridorSearchStats), RoutingError> {
        let (transit_corridors, stats) = corridors::generate(&self.transit, query)?;
        tracing::Span::current()
            .record("states_expanded", stats.states_expanded)
            .record("corridor_count", stats.corridors_retained);
        if query.deadline.is_elapsed() {
            return Err(RoutingError::DeadlineExceeded);
        }
        tracing::debug!(
            target: "via_server::routing",
            states_expanded = stats.states_expanded,
            corridors_found = stats.unique_corridors_found,
            corridors_retained = stats.corridors_retained,
            latency_ms = stats.search_latency_ms,
            "static round-based corridor search"
        );
        Ok((transit_corridors, stats))
    }

    fn timing_positions(&self, corridors: &[Corridor]) -> (Vec<PatternPosition>, usize) {
        let mut positions = HashSet::new();
        let mut pattern_ids = HashSet::new();
        let mut bus_stop_ids = HashSet::new();
        for leg in corridors.iter().flat_map(Corridor::transit_legs) {
            positions.insert(PatternPosition {
                pattern_id: leg.pattern_id.clone(),
                stop_index: leg.board_index,
            });
            pattern_ids.insert(&leg.pattern_id);
            let pattern = self.transit.required_pattern(&leg.pattern_id);
            if self.transit.required_route(&pattern.route_id).mode == TransitMode::Bus {
                bus_stop_ids.insert(&leg.board_stop_id);
            }
        }
        let mut positions = positions.into_iter().collect::<Vec<_>>();
        positions.sort_by(|left, right| {
            left.pattern_id
                .as_str()
                .cmp(right.pattern_id.as_str())
                .then(left.stop_index.cmp(&right.stop_index))
        });
        ::metrics::counter!("via_planner_transit_reachable_boardings_total")
            .increment(positions.len() as u64);
        ::metrics::counter!("via_planner_transit_reachable_patterns_total")
            .increment(pattern_ids.len() as u64);
        ::metrics::counter!("via_planner_transit_realtime_stops_requested_total")
            .increment(bus_stop_ids.len() as u64);
        (positions, bus_stop_ids.len())
    }

    #[tracing::instrument(target = "via_profile", name = "Snapshot Loading", skip_all, fields(corridor_count = corridors.iter().filter(|corridor| corridor.transit_legs().next().is_some()).count()))]
    async fn load_snapshots(
        &self,
        query: &PlanningQuery,
        corridors: &[Corridor],
    ) -> Result<(RequestSnapshots, FailureSummary), RoutingError> {
        let window_end = query.departure_time
            + Duration::seconds(i64::from(query.limits.max_journey_duration_seconds));
        let departures_per_position = usize::try_from(query.limits.max_journey_duration_seconds)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .max(query.limits.max_departures_per_query);
        let (timing_positions, bus_stop_count) = self.timing_positions(corridors);
        let timing_queries = timing_positions
            .into_iter()
            .map(|position| {
                let stop_id = self
                    .transit
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
            self.bike_planner
                .required_station_ids(corridors.iter().flat_map(Corridor::anchors))
        } else {
            Vec::new()
        };
        let positions_requested = timing_queries.len();
        let started = Instant::now();
        let timing = async {
            if timing_queries.is_empty() {
                TimingSnapshot::default()
            } else {
                self.timing_loader.load_snapshot(timing_queries).await
            }
        };
        let bike_availability = async {
            if query.allow_bike_share {
                Some(
                    self.bike_availability
                        .load_snapshot(&bike_station_ids)
                        .await,
                )
            } else {
                None
            }
        };
        let (timing, bike_availability) = tokio::join!(
            timing.instrument(tracing::info_span!(target: "via_profile", "TDX Timing")),
            bike_availability.instrument(tracing::info_span!(
                target: "via_profile",
                "BikeShare Availability",
                enabled = query.allow_bike_share
            ))
        );
        let mut failures = FailureSummary::default();
        let bike_availability = match bike_availability {
            None => None,
            Some(Ok(availability)) => Some(availability),
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
            corridor_bus_stops = bus_stop_count,
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
        Ok((
            RequestSnapshots {
                timing,
                bike_availability,
            },
            failures,
        ))
    }
}
