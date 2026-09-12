//! Phase 3: expand only local BikeShare connectors around timed transit
//! access/egress gaps.  No station is added to the graph until it survives
//! this spatial and competitive pruning step.

use std::collections::HashSet;

use crate::{
    api::{JourneyMode, PlanningRequest, RequestCoordinate},
    normalization::BoardingPointId,
    planner::{PlanningContext, PlanningLimits, StreetMetric, planner_coordinate, street_metric},
    street::{
        MatrixOrientation, MatrixQuery, ProviderError, RoutingConstraints, StreetRoutingSession,
        StreetTravelMode,
    },
    transit::{BikeShareStation, TransitNetwork},
};

use super::{candidate_discovery::TransitCandidateNetwork, transit_timing::TimedTransitBackbone};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BikeShareGap {
    OriginToTransit(BoardingPointId),
    TransitToDestination(BoardingPointId),
    Direct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareOpportunity {
    pub gap: BikeShareGap,
    pub pickup_station_id: crate::normalization::BikeShareStationId,
    pub return_station_id: crate::normalization::BikeShareStationId,
    pub access: StreetMetric,
    pub bicycle: StreetMetric,
    pub egress: StreetMetric,
    pub total_seconds: u32,
}

#[derive(Clone, Debug, Default)]
pub struct BikeShareExpansion {
    pub opportunities: Vec<BikeShareOpportunity>,
    pub station_ids: Vec<crate::normalization::BikeShareStationId>,
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
        limits: &PlanningLimits,
        candidate: &TransitCandidateNetwork,
        timed: &TimedTransitBackbone,
    ) -> Result<BikeShareExpansion, ProviderError> {
        if !request.selected_modes.contains(&JourneyMode::BikeShare) {
            return Ok(BikeShareExpansion::default());
        }
        let mut gaps = Vec::new();
        let mut seen = HashSet::new();
        if timed.itineraries.is_empty() {
            gaps.push(BikeShareGap::Direct);
        } else {
            for itinerary in &timed.itineraries {
                if let Some(first) = itinerary.rides.first()
                    && let Some(access) = candidate.access_for(&first.ride.board_stop_id)
                    && access.metric.duration_seconds >= limits.min_walking_before_bike_seconds
                    && seen.insert(BikeShareGap::OriginToTransit(
                        first.ride.board_stop_id.clone(),
                    ))
                {
                    gaps.push(BikeShareGap::OriginToTransit(
                        first.ride.board_stop_id.clone(),
                    ));
                }
                if let Some(last) = itinerary.rides.last()
                    && let Some(egress) = candidate.egress_for(&last.ride.alight_stop_id)
                    && egress.metric.duration_seconds >= limits.min_walking_before_bike_seconds
                    && seen.insert(BikeShareGap::TransitToDestination(
                        last.ride.alight_stop_id.clone(),
                    ))
                {
                    gaps.push(BikeShareGap::TransitToDestination(
                        last.ride.alight_stop_id.clone(),
                    ));
                }
            }
        }
        gaps.sort_by_key(gap_sort_key);

        let origin = planner_coordinate(request.origin.coordinate)?;
        let destination = planner_coordinate(request.destination.coordinate)?;
        let mut opportunities = Vec::new();
        let mut stations_considered = 0;
        for gap in gaps {
            let (from, to, baseline) = match &gap {
                BikeShareGap::OriginToTransit(stop_id) => {
                    let target = self
                        .network
                        .stop(stop_id)
                        .ok_or(ProviderError::UnreachableEndpoints)?;
                    let baseline = candidate
                        .access_for(stop_id)
                        .map(|access| access.metric.duration_seconds)
                        .unwrap_or(u32::MAX);
                    (origin, planner_coordinate(target.coordinate)?, baseline)
                }
                BikeShareGap::TransitToDestination(stop_id) => {
                    let source = self
                        .network
                        .stop(stop_id)
                        .ok_or(ProviderError::UnreachableEndpoints)?;
                    let baseline = candidate
                        .egress_for(stop_id)
                        .map(|egress| egress.metric.duration_seconds)
                        .unwrap_or(u32::MAX);
                    (
                        planner_coordinate(source.coordinate)?,
                        destination,
                        baseline,
                    )
                }
                BikeShareGap::Direct => (origin, destination, u32::MAX),
            };
            let mut pickup_center = from;
            let mut return_center = to;
            if let BikeShareGap::OriginToTransit(stop_id) = &gap {
                return_center = planner_coordinate(
                    self.network
                        .stop(stop_id)
                        .ok_or(ProviderError::UnreachableEndpoints)?
                        .coordinate,
                )?;
            }
            if let BikeShareGap::TransitToDestination(stop_id) = &gap {
                pickup_center = planner_coordinate(
                    self.network
                        .stop(stop_id)
                        .ok_or(ProviderError::UnreachableEndpoints)?
                        .coordinate,
                )?;
            }
            let pickups = nearby_stations(
                self.network,
                pickup_center,
                limits.max_bike_pickup_stations,
                limits.max_bike_search_radius_metres,
            );
            let returns = nearby_stations(
                self.network,
                return_center,
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
                    if pickup.id == returns[return_index].id
                        || bicycle.duration_seconds > limits.max_bike_duration_seconds
                    {
                        continue;
                    }
                    let Some(egress) = egress_metrics.get(return_index).copied().flatten() else {
                        continue;
                    };
                    let total_seconds = access
                        .duration_seconds
                        .saturating_add(bicycle.duration_seconds)
                        .saturating_add(egress.duration_seconds);
                    let competitive_limit = baseline.saturating_add(limits.max_bike_extra_seconds);
                    if !matches!(gap, BikeShareGap::Direct) && total_seconds > competitive_limit {
                        continue;
                    }
                    opportunities.push(BikeShareOpportunity {
                        gap: gap.clone(),
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
                .then(gap_sort_key(&left.gap).cmp(&gap_sort_key(&right.gap)))
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
            left.gap == right.gap
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
        source: crate::street::Coordinate,
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
        target: crate::street::Coordinate,
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

fn nearby_stations(
    network: &TransitNetwork,
    coordinate: crate::street::Coordinate,
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

fn gap_sort_key(gap: &BikeShareGap) -> String {
    match gap {
        BikeShareGap::OriginToTransit(stop_id) => format!("origin:{}", stop_id.as_str()),
        BikeShareGap::TransitToDestination(stop_id) => format!("destination:{}", stop_id.as_str()),
        BikeShareGap::Direct => "direct".into(),
    }
}

fn distance_metres(from: RequestCoordinate, to: RequestCoordinate) -> u64 {
    let latitude = ((from.latitude + to.latitude) / 2.0).to_radians();
    let north = (to.latitude - from.latitude) * 111_320.0;
    let east = (to.longitude - from.longitude) * 111_320.0 * latitude.cos();
    north.hypot(east).max(0.0).round() as u64
}
