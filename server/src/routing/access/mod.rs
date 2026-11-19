mod nearby;

use std::{sync::Arc, time::Instant};

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

use super::model::{EndpointStop, FailureSummary, JourneyPlanningContext, JourneyPlanningLimits};

#[derive(Clone)]
pub(crate) struct AccessDiscovery {
    network: Arc<TransitRuntime>,
    street: Arc<dyn StreetRouter>,
}

impl AccessDiscovery {
    pub(crate) fn new(network: Arc<TransitRuntime>, street: Arc<dyn StreetRouter>) -> Self {
        Self { network, street }
    }

    async fn discover_endpoint_stops(
        &self,
        endpoint: GeoCoordinate,
        orientation: MatrixOrientation,
        constraints: RoutingConstraints,
        allowed_transit_modes: &[TransitMode],
        limits: &JourneyPlanningLimits,
    ) -> Result<Vec<EndpointStop>, StreetRoutingError> {
        let endpoint = Coordinate::try_new(endpoint.latitude, endpoint.longitude)?;
        let stop_limit = limits.max_transit_endpoint_stops;
        let stops = nearby::nearby_stops(
            &self.network,
            endpoint,
            allowed_transit_modes,
            stop_limit,
            limits,
        );
        if stops.is_empty() {
            return Ok(Vec::new());
        }
        let coordinates = stops.iter().map(|(_, coordinate)| *coordinate).collect();
        let (sources, targets) = match orientation {
            MatrixOrientation::OneToMany => (vec![endpoint], coordinates),
            _ => (coordinates, vec![endpoint]),
        };
        let matrix = self
            .street
            .matrix(MatrixQuery {
                orientation,
                travel_mode: StreetTravelMode::Walking,
                sources,
                targets,
                constraints,
            })
            .await?;
        let cells: Vec<Option<&MatrixCell>> = match orientation {
            MatrixOrientation::OneToMany => matrix
                .cells
                .first()
                .into_iter()
                .flatten()
                .map(Some)
                .collect(),
            _ => matrix.cells.iter().map(|row| row.first()).collect(),
        };
        let mut walks = cells
            .into_iter()
            .zip(stops)
            .filter_map(|(cell, (stop_id, _))| {
                let metric = nearby::street_metric(cell?)?;
                (metric.duration_seconds <= limits.max_walking_seconds).then(|| {
                    EndpointStop::new(stop_id, metric.duration_seconds, metric.distance_metres)
                })
            })
            .collect::<Vec<_>>();
        walks.sort_by(|left, right| {
            left.walking_seconds
                .cmp(&right.walking_seconds)
                .then(left.distance_metres.cmp(&right.distance_metres))
                .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
        });
        Ok(nearby::prune_coverage(&self.network, walks, stop_limit))
    }

    pub(crate) async fn discover(
        &self,
        context: &JourneyPlanningContext<'_>,
        limits: &JourneyPlanningLimits,
        modes: &[TransitMode],
        failures: &mut FailureSummary,
    ) -> Result<(Vec<EndpointStop>, Vec<EndpointStop>), PlanningError> {
        if modes.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let request = context.request;
        let started = Instant::now();
        let access = self
            .discover_endpoint_stops(
                request.origin.coordinate,
                MatrixOrientation::OneToMany,
                context.departure_constraints(),
                modes,
                limits,
            )
            .await;
        context.ensure_before_deadline()?;
        let egress = self
            .discover_endpoint_stops(
                request.destination.coordinate,
                MatrixOrientation::ManyToOne,
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
                nearby::modes_at_stop(&self.network, stop_id),
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
