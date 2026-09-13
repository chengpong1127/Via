//! Generic BikeShare substitution for eligible walking edges.

use crate::{
    api::PlanningRequest,
    domain::transit::{BikeShareStation, TransitNetwork},
    domain::{ids::BikeShareStationId, journey::JourneyMode, location::RequestCoordinate},
    routing::service::{PlanningContext, RoutingLimits, planner_coordinate, street_metric},
    street::{
        Coordinate, MatrixOrientation, MatrixQuery, ProviderError, RoutingConstraints,
        StreetRoutingSession, StreetTravelMode,
    },
};

use super::graph::{RouteEdge, RouteGraph, RouteNode, StreetMetric};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareSegment {
    pub from: RouteNode,
    pub to: RouteNode,
    /// An absent baseline means this is only an expansion seed; it is never
    /// inserted as a walking edge in the route graph.
    pub baseline_seconds: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareOpportunity {
    pub segment: BikeShareSegment,
    pub pickup_station_id: BikeShareStationId,
    pub return_station_id: BikeShareStationId,
    pub access: StreetMetric,
    pub bicycle: StreetMetric,
    pub egress: StreetMetric,
    pub total_seconds: u32,
}

#[derive(Clone, Debug, Default)]
pub struct BikeShareExpansion {
    /// Only station IDs that survived spatial and competitive pruning are
    /// returned for demand-driven realtime enrichment.
    pub opportunities: Vec<BikeShareOpportunity>,
    pub station_ids: Vec<BikeShareStationId>,
    pub stations_considered: usize,
}

impl BikeShareExpansion {
    pub fn is_empty(&self) -> bool {
        self.opportunities.is_empty()
    }
}

#[derive(Clone)]
pub struct BikeShareOpportunityDiscovery<'a> {
    street_routing: &'a StreetRoutingSession,
    network: &'a TransitNetwork,
}

impl<'a> BikeShareOpportunityDiscovery<'a> {
    pub fn new(street_routing: &'a StreetRoutingSession, network: &'a TransitNetwork) -> Self {
        Self {
            street_routing,
            network,
        }
    }

    pub(crate) async fn discover(
        &self,
        request: &PlanningRequest,
        context: &PlanningContext,
        limits: &RoutingLimits,
        base: &RouteGraph,
    ) -> Result<BikeShareExpansion, ProviderError> {
        if !request.selected_modes.contains(&JourneyMode::BikeShare) {
            return Ok(BikeShareExpansion::default());
        }

        let mut segments = base
            .edges
            .iter()
            .filter_map(|edge| match edge {
                RouteEdge::Walk { from, to, metric } => Some(BikeShareSegment {
                    from: from.clone(),
                    to: to.clone(),
                    baseline_seconds: Some(metric.duration_seconds),
                }),
                _ => None,
            })
            .filter(|segment| {
                segment
                    .baseline_seconds
                    .is_some_and(|seconds| seconds >= limits.min_walking_before_bike_seconds)
            })
            .collect::<Vec<_>>();
        segments.sort_by_key(segment_key);
        segments.dedup();
        // A direct walk may be unreachable while both endpoint station
        // connectors remain routable. Keep that BikeShare route discoverable
        // without inventing a walking edge or geographic walking metric.
        let segments = if segments.is_empty()
            && base.nodes.contains(&RouteNode::Origin)
            && base.nodes.contains(&RouteNode::Destination)
        {
            vec![BikeShareSegment {
                from: RouteNode::Origin,
                to: RouteNode::Destination,
                baseline_seconds: None,
            }]
        } else {
            segments
        };

        let mut opportunities = Vec::new();
        let mut stations_considered = 0;
        for segment in segments {
            let Some(from) = node_coordinate(request, self.network, &segment.from) else {
                continue;
            };
            let Some(to) = node_coordinate(request, self.network, &segment.to) else {
                continue;
            };
            let pickups = nearby_stations(
                self.network,
                from,
                limits.max_bike_pickup_stations,
                limits.max_bike_search_radius_metres,
            );
            let returns = nearby_stations(
                self.network,
                to,
                limits.max_bike_return_stations,
                limits.max_bike_search_radius_metres,
            );
            stations_considered += pickups.len() + returns.len();
            if pickups.is_empty() || returns.is_empty() {
                continue;
            }

            let access_metrics = self.walking_one_to_many(from, &pickups, context).await?;
            let egress_metrics = self.walking_many_to_one(&returns, to, context).await?;
            for (pickup_index, pickup) in pickups.iter().enumerate() {
                let Some(access) = access_metrics.get(pickup_index).copied().flatten() else {
                    continue;
                };
                let bike_matrix = self
                    .street_routing
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::OneToMany,
                        travel_mode: StreetTravelMode::Bicycle,
                        sources: vec![planner_coordinate(pickup.coordinate)?],
                        targets: returns
                            .iter()
                            .map(|station| planner_coordinate(station.coordinate))
                            .collect::<Result<Vec<_>, _>>()?,
                        constraints: RoutingConstraints {
                            max_distance: Some(crate::street::Metres(
                                limits.max_bike_distance_metres,
                            )),
                            ..context.constraints(
                                context.search_start,
                                crate::street::RoutingTimeKind::DepartAt,
                            )
                        },
                    })
                    .await?;
                for (return_index, cell) in
                    bike_matrix.cells.first().into_iter().flatten().enumerate()
                {
                    let Some(bicycle) = street_metric(cell) else {
                        continue;
                    };
                    let Some(egress) = egress_metrics.get(return_index).copied().flatten() else {
                        continue;
                    };
                    if pickup.id == returns[return_index].id
                        || bicycle.duration_seconds > limits.max_bike_duration_seconds
                        || bicycle.distance_metres > limits.max_bike_distance_metres
                    {
                        continue;
                    }
                    let total_seconds = access
                        .duration_seconds
                        .saturating_add(bicycle.duration_seconds)
                        .saturating_add(egress.duration_seconds);
                    if let Some(baseline_seconds) = segment.baseline_seconds {
                        let competitive_limit =
                            baseline_seconds.saturating_add(limits.max_bike_extra_seconds);
                        if total_seconds > competitive_limit {
                            continue;
                        }
                    }
                    opportunities.push(BikeShareOpportunity {
                        segment: segment.clone(),
                        pickup_station_id: pickup.id.clone(),
                        return_station_id: returns[return_index].id.clone(),
                        access,
                        bicycle,
                        egress,
                        total_seconds,
                    });
                }
            }
        }

        opportunities.sort_by(|left, right| {
            left.total_seconds
                .cmp(&right.total_seconds)
                .then(segment_key(&left.segment).cmp(&segment_key(&right.segment)))
                .then(
                    left.pickup_station_id
                        .as_str()
                        .cmp(right.pickup_station_id.as_str()),
                )
                .then(
                    left.return_station_id
                        .as_str()
                        .cmp(right.return_station_id.as_str()),
                )
        });
        opportunities.dedup_by(|left, right| {
            left.segment == right.segment
                && left.pickup_station_id == right.pickup_station_id
                && left.return_station_id == right.return_station_id
        });
        opportunities.truncate(limits.max_bike_connectors.max(1));
        let mut station_ids = opportunities
            .iter()
            .flat_map(|opportunity| {
                [
                    opportunity.pickup_station_id.clone(),
                    opportunity.return_station_id.clone(),
                ]
            })
            .collect::<Vec<_>>();
        station_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        station_ids.dedup();
        Ok(BikeShareExpansion {
            opportunities,
            station_ids,
            stations_considered,
        })
    }

    async fn walking_one_to_many(
        &self,
        source: Coordinate,
        targets: &[BikeShareStation],
        context: &PlanningContext,
    ) -> Result<Vec<Option<StreetMetric>>, ProviderError> {
        let matrix = self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![source],
                targets: targets
                    .iter()
                    .map(|station| planner_coordinate(station.coordinate))
                    .collect::<Result<Vec<_>, _>>()?,
                constraints: context.access_constraints(),
            })
            .await?;
        Ok(matrix
            .cells
            .first()
            .into_iter()
            .flatten()
            .map(street_metric)
            .collect())
    }

    async fn walking_many_to_one(
        &self,
        sources: &[BikeShareStation],
        target: Coordinate,
        context: &PlanningContext,
    ) -> Result<Vec<Option<StreetMetric>>, ProviderError> {
        let matrix = self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToOne,
                travel_mode: StreetTravelMode::Walking,
                sources: sources
                    .iter()
                    .map(|station| planner_coordinate(station.coordinate))
                    .collect::<Result<Vec<_>, _>>()?,
                targets: vec![target],
                constraints: context.direct_constraints(),
            })
            .await?;
        Ok(matrix
            .cells
            .iter()
            .map(|cells| cells.first().and_then(street_metric))
            .collect())
    }
}

fn node_coordinate(
    request: &PlanningRequest,
    network: &TransitNetwork,
    node: &RouteNode,
) -> Option<Coordinate> {
    let coordinate = match node {
        RouteNode::Origin => request.origin.coordinate,
        RouteNode::Destination => request.destination.coordinate,
        RouteNode::TransitStop(stop_id) => network.stop(stop_id)?.coordinate,
        RouteNode::BikeShareStation(_) => return None,
    };
    Coordinate::try_new(coordinate.latitude, coordinate.longitude).ok()
}

fn nearby_stations(
    network: &TransitNetwork,
    coordinate: Coordinate,
    limit: usize,
    radius_metres: u32,
) -> Vec<BikeShareStation> {
    let request_coordinate = RequestCoordinate {
        latitude: coordinate.latitude(),
        longitude: coordinate.longitude(),
    };
    network
        .nearest_bike_share_stations(request_coordinate, limit)
        .into_iter()
        .filter(|station| {
            distance_metres(request_coordinate, station.coordinate) <= u64::from(radius_metres)
        })
        .cloned()
        .collect()
}

fn segment_key(segment: &BikeShareSegment) -> String {
    format!(
        "{}:{}:{}",
        node_key(&segment.from),
        node_key(&segment.to),
        segment.baseline_seconds.unwrap_or_default()
    )
}

fn node_key(node: &RouteNode) -> String {
    match node {
        RouteNode::Origin => "origin".into(),
        RouteNode::Destination => "destination".into(),
        RouteNode::TransitStop(id) => format!("stop:{}", id.as_str()),
        RouteNode::BikeShareStation(id) => format!("station:{}", id.as_str()),
    }
}

fn distance_metres(from: RequestCoordinate, to: RequestCoordinate) -> u64 {
    let latitude = ((from.latitude + to.latitude) / 2.0).to_radians();
    let north = (to.latitude - from.latitude) * 111_320.0;
    let east = (to.longitude - from.longitude) * 111_320.0 * latitude.cos();
    north.hypot(east).max(0.0).round() as u64
}
