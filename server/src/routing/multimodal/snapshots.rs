use super::*;

impl MultimodalPlanner {
    #[tracing::instrument(target = "via_profile", name = "Snapshot Loading", skip_all, fields(corridor_count = retained_corridors.retained().len(), bike_station_count = bike_station_ids.len()))]
    pub(super) async fn load_request_snapshots(
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
        let mut failures = crate::routing::failures::FailureSummary::default();
        let timing_future = async {
            if timing_queries.is_empty() {
                crate::domain::transit_timing::TimingSnapshot::default()
            } else {
                self.timing.load_snapshot(timing_queries).await
            }
        };
        let bike_future = async {
            if bike_enabled {
                Some(self.bike_availability.snapshot(bike_station_ids).await)
            } else {
                None
            }
        };
        use tracing::Instrument;
        let (snapshot, bike_result) = tokio::join!(
            timing_future.instrument(tracing::info_span!(target: "via_profile", "TDX Timing")),
            bike_future.instrument(tracing::info_span!(target: "via_profile", "BikeShare Availability", enabled = bike_enabled))
        );
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
