use super::*;

impl MultimodalPlanner {
    pub(crate) async fn discover_access_points(
        &self,
        origin: GeoCoordinate,
        departure_time: OffsetDateTime,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<AccessPoint>, StreetRoutingError> {
        let origin = Coordinate::try_new(origin.latitude, origin.longitude)?;
        let stops = crate::routing::access::nearby_stops(
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
        let matrix = self
            .street
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
                let metric = crate::routing::access::street_metric(cell)?;
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
        Ok(crate::routing::access::prune_access_coverage(
            &self.topology,
            access_points,
            limits.max_transit_access_stops,
        ))
    }

    pub(crate) async fn discover_egress_points(
        &self,
        destination: GeoCoordinate,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<EgressPoint>, StreetRoutingError> {
        let destination = Coordinate::try_new(destination.latitude, destination.longitude)?;
        let stops = crate::routing::access::nearby_stops(
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
        let matrix = self
            .street
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
                let metric = crate::routing::access::street_metric(cells.first()?)?;
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
        Ok(crate::routing::access::prune_egress_coverage(
            &self.topology,
            egress_points,
            limits.max_transit_egress_stops,
        ))
    }

    pub(crate) fn modes_at_stop(&self, stop_id: &BoardingPointId) -> Vec<TransitMode> {
        crate::routing::access::modes_at_stop(&self.network, &self.topology, stop_id)
    }
}

impl MultimodalPlanner {
    pub(crate) async fn discover_query(
        &self,
        context: &crate::routing::model::JourneyPlanningContext<'_>,
        limits: &JourneyPlanningLimits,
    ) -> Result<
        (PlanningQuery, crate::routing::failures::FailureSummary),
        crate::domain::journey::PlanningError,
    > {
        use crate::domain::journey::PlanningError;
        use crate::routing::{failures::FailureSummary, reconstruct::to_street_coordinate};
        let request = context.request;
        let allowed_transit_modes = request.transit_modes.clone();
        let mut failures = FailureSummary::default();

        let (origin, destination) = (
            to_street_coordinate(request.origin.coordinate).map_err(|_| PlanningError::Internal)?,
            to_street_coordinate(request.destination.coordinate)
                .map_err(|_| PlanningError::Internal)?,
        );
        use tracing::Instrument;
        let query = async {
            Ok::<_, PlanningError>(if allowed_transit_modes.is_empty() {
                PlanningQuery::new(
                    origin,
                    destination,
                    context.deadline,
                    Vec::new(),
                    Vec::new(),
                    context.search_start,
                    limits.planner,
                )
                .with_allowed_transit_modes([])
            } else {
                let access_started = Instant::now();
                let access = self
                    .discover_access_points(
                        request.origin.coordinate,
                        context.search_start,
                        context.access_constraints(),
                        &allowed_transit_modes,
                        limits,
                    )
                    .await;
                context.ensure_before_deadline()?;
                let egress = self
                    .discover_egress_points(
                        request.destination.coordinate,
                        context.direct_constraints(),
                        &allowed_transit_modes,
                        limits,
                    )
                    .await;
                context.ensure_before_deadline()?;
                let (access, egress) = match (access, egress) {
                    (Ok(access), Ok(egress)) => (access, egress),
                    (Err(error), _) | (_, Err(error)) => {
                        failures.record_provider(&error);
                        (Vec::new(), Vec::new())
                    }
                };
                let candidate_stops = access.len() + egress.len();
                let discovery_duration = access_started.elapsed();
                ::metrics::counter!("via_planner_transit_candidate_stops_total")
                    .increment(candidate_stops as u64);
                ::metrics::histogram!("via_planner_candidate_discovery_duration_seconds")
                    .record(discovery_duration.as_secs_f64());
                tracing::debug!(
                    target: "via_server::routing",
                    candidate_discovery_ms = discovery_duration.as_millis() as u64,
                    transit_candidate_stops = candidate_stops,
                    "transit candidate discovery completed"
                );
                tracing::debug!(
                    target: "via_server::routing",
                    access = ?access.iter().map(|point| (
                        point.stop_id.as_str(),
                        self.modes_at_stop(&point.stop_id),
                        point.walking_seconds,
                    )).collect::<Vec<_>>(),
                    egress = ?egress.iter().map(|point| (
                        point.stop_id.as_str(),
                        self.modes_at_stop(&point.stop_id),
                        point.walking_seconds,
                    )).collect::<Vec<_>>(),
                    "discovered transit access and egress"
                );

                PlanningQuery::new(
                    origin,
                    destination,
                    context.deadline,
                    access,
                    egress,
                    context.search_start,
                    limits.planner,
                )
                .with_allowed_transit_modes(allowed_transit_modes)
            })
        }
        .instrument(tracing::info_span!(target: "via_profile", "Candidate Discovery"))
        .await?;
        let mut query = query
            .with_bike_share(request.bike_share)
            .with_limits(limits.planner);
        query.latest_arrival = context.latest_arrival;

        Ok((query, failures))
    }
}
