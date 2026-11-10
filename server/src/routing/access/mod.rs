mod nearby;

use std::{sync::Arc, time::Instant};

use time::{Duration, OffsetDateTime};

use crate::{
    domain::{
        ids::BoardingPointId, journey::PlanningError, location::GeoCoordinate,
        transit::TransitMode, transit_runtime::TransitRuntime,
    },
    street::{
        Coordinate, MatrixCell, MatrixOrientation, MatrixQuery, RoutingConstraints, StreetRouter,
        StreetRoutingError, StreetTravelMode,
    },
};

use super::{
    corridors::TransitTopologyIndex,
    model::{
        AccessPoint, EgressPoint, FailureSummary, JourneyPlanningContext, JourneyPlanningLimits,
    },
};

#[derive(Clone)]
pub(crate) struct AccessDiscovery {
    network: Arc<TransitRuntime>,
    topology: Arc<TransitTopologyIndex>,
    street: Arc<dyn StreetRouter>,
}

impl AccessDiscovery {
    pub(crate) fn new(
        network: Arc<TransitRuntime>,
        topology: Arc<TransitTopologyIndex>,
        street: Arc<dyn StreetRouter>,
    ) -> Self {
        Self {
            network,
            topology,
            street,
        }
    }

    async fn discover_access_points(
        &self,
        origin: GeoCoordinate,
        departure_time: OffsetDateTime,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<AccessPoint>, StreetRoutingError> {
        let walks = self
            .walks_to_nearby_stops(
                origin,
                true,
                constraints,
                allowed_transit_modes,
                limits.max_transit_access_stops,
                limits,
            )
            .await?;
        let points = walks
            .into_iter()
            .map(|(stop_id, metric)| {
                AccessPoint::new(
                    stop_id,
                    departure_time,
                    departure_time + Duration::seconds(i64::from(metric.duration_seconds)),
                    metric.duration_seconds,
                    metric.distance_metres,
                )
            })
            .collect();
        Ok(nearby::prune_access_coverage(
            &self.topology,
            points,
            limits.max_transit_access_stops,
        ))
    }

    async fn discover_egress_points(
        &self,
        destination: GeoCoordinate,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<EgressPoint>, StreetRoutingError> {
        let walks = self
            .walks_to_nearby_stops(
                destination,
                false,
                constraints,
                allowed_transit_modes,
                limits.max_transit_egress_stops,
                limits,
            )
            .await?;
        let points = walks
            .into_iter()
            .map(|(stop_id, metric)| {
                EgressPoint::new(stop_id, metric.duration_seconds, metric.distance_metres)
            })
            .collect();
        Ok(nearby::prune_egress_coverage(
            &self.topology,
            points,
            limits.max_transit_egress_stops,
        ))
    }

    async fn walks_to_nearby_stops(
        &self,
        endpoint: GeoCoordinate,
        from_endpoint: bool,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        stop_limit: usize,
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<(BoardingPointId, nearby::StreetMetric)>, StreetRoutingError> {
        let endpoint = Coordinate::try_new(endpoint.latitude, endpoint.longitude)?;
        let stops = nearby::nearby_stops(
            &self.network,
            &self.topology,
            endpoint,
            allowed_transit_modes,
            stop_limit,
            limits,
        );
        if stops.is_empty() {
            return Ok(Vec::new());
        }
        let coordinates = stops.iter().map(|(_, coordinate)| *coordinate).collect();
        let matrix = self
            .street
            .matrix(if from_endpoint {
                MatrixQuery {
                    orientation: MatrixOrientation::OneToMany,
                    travel_mode: StreetTravelMode::Walking,
                    sources: vec![endpoint],
                    targets: coordinates,
                    constraints,
                }
            } else {
                MatrixQuery {
                    orientation: MatrixOrientation::ManyToOne,
                    travel_mode: StreetTravelMode::Walking,
                    sources: coordinates,
                    targets: vec![endpoint],
                    constraints,
                }
            })
            .await?;
        let cells: Vec<Option<&MatrixCell>> = if from_endpoint {
            matrix
                .cells
                .first()
                .into_iter()
                .flatten()
                .map(Some)
                .collect()
        } else {
            matrix.cells.iter().map(|row| row.first()).collect()
        };
        let mut walks = cells
            .into_iter()
            .zip(&stops)
            .filter_map(|(cell, (stop_id, _))| {
                let metric = nearby::street_metric(cell?)?;
                (metric.duration_seconds <= limits.max_walking_seconds)
                    .then(|| (stop_id.clone(), metric))
            })
            .collect::<Vec<_>>();
        walks.sort_by(|(left_id, left), (right_id, right)| {
            left.duration_seconds
                .cmp(&right.duration_seconds)
                .then(left.distance_metres.cmp(&right.distance_metres))
                .then(left_id.as_str().cmp(right_id.as_str()))
        });
        Ok(walks)
    }

    pub(crate) async fn discover(
        &self,
        context: &JourneyPlanningContext<'_>,
        limits: &JourneyPlanningLimits,
        modes: &[TransitMode],
        failures: &mut FailureSummary,
    ) -> Result<(Vec<AccessPoint>, Vec<EgressPoint>), PlanningError> {
        let request = context.request;
        let started = Instant::now();
        let access = self
            .discover_access_points(
                request.origin.coordinate,
                context.search_start,
                context.departure_constraints(),
                modes,
                limits,
            )
            .await;
        context.ensure_before_deadline()?;
        let egress = self
            .discover_egress_points(
                request.destination.coordinate,
                context.departure_constraints(),
                modes,
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
        let duration = started.elapsed();
        ::metrics::counter!("via_planner_transit_candidate_stops_total")
            .increment(candidate_stops as u64);
        ::metrics::histogram!("via_planner_candidate_discovery_duration_seconds")
            .record(duration.as_secs_f64());
        let describe = |stop_id: &BoardingPointId, walking_seconds: u32| {
            (
                stop_id.as_str().to_owned(),
                nearby::modes_at_stop(&self.network, &self.topology, stop_id),
                walking_seconds,
            )
        };
        tracing::debug!(
            target: "via_server::routing",
            candidate_discovery_ms = duration.as_millis() as u64,
            transit_candidate_stops = candidate_stops,
            access = ?access.iter().map(|point| describe(&point.stop_id, point.walking_seconds)).collect::<Vec<_>>(),
            egress = ?egress.iter().map(|point| describe(&point.stop_id, point.walking_seconds)).collect::<Vec<_>>(),
            "transit candidate discovery completed"
        );
        Ok((access, egress))
    }
}
