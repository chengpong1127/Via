//! Phase 1: find the small, deterministic transit topology relevant to one
//! Planning Request.  This module deliberately knows nothing about BikeShare
//! stations or realtime values.

use std::collections::{HashSet, VecDeque};

use crate::{
    api::PlanningRequest,
    domain::transit::{TransferEdge, TransitAccess, TransitNetwork, TransitTopologyItinerary},
    domain::{
        ids::{BoardingPointId, TransitServiceId},
        journey::JourneyMode,
        location::RequestCoordinate,
        transit::TransitRide,
    },
    routing::service::{PlanningContext, RoutingLimits, planner_coordinate, street_metric},
    street::{
        MatrixOrientation, MatrixQuery, ProviderError, StreetRoutingSession, StreetTravelMode,
    },
};

use super::graph::StreetMetric;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitAccessCandidate {
    pub access: TransitAccess,
    pub metric: StreetMetric,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitEgressCandidate {
    pub stop_id: BoardingPointId,
    pub metric: StreetMetric,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryResult {
    pub boarding_points: Vec<BoardingPointId>,
    pub services: Vec<TransitServiceId>,
    pub transfer_edges: Vec<TransferEdge>,
    pub origin_access: Vec<TransitAccessCandidate>,
    pub destination_egress: Vec<TransitEgressCandidate>,
    /// Untimed paths found by destination-oriented service-pattern expansion.
    pub topology_itineraries: Vec<TransitTopologyItinerary>,
    /// The direct walking edge is part of the base network when street routing
    /// can reach the destination.
    pub direct_walking: Option<StreetMetric>,
}

impl DiscoveryResult {
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
            || self.origin_access.is_empty()
            || self.destination_egress.is_empty()
    }

    pub fn access_for(&self, stop_id: &BoardingPointId) -> Option<&TransitAccessCandidate> {
        self.origin_access
            .iter()
            .find(|access| &access.access.stop_id == stop_id)
    }

    pub fn egress_for(&self, stop_id: &BoardingPointId) -> Option<&TransitEgressCandidate> {
        self.destination_egress
            .iter()
            .find(|egress| &egress.stop_id == stop_id)
    }
}

#[derive(Clone)]
pub struct Discovery<'a> {
    street_routing: &'a StreetRoutingSession,
    network: &'a TransitNetwork,
}

impl<'a> Discovery<'a> {
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
    ) -> Result<DiscoveryResult, ProviderError> {
        if !request.selected_modes.contains(&JourneyMode::Transit) {
            return Ok(DiscoveryResult::default());
        }
        let origin = planner_coordinate(request.origin.coordinate)?;
        let destination = planner_coordinate(request.destination.coordinate)?;

        let origin_pool = self
            .network
            .nearest_stops(
                request.origin.coordinate,
                limits.max_origin_side_stops.max(1),
            )
            .into_iter()
            .filter(|stop| {
                geographic_distance_metres(request.origin.coordinate, stop.coordinate)
                    <= u64::from(limits.max_walking_radius_metres)
            })
            .map(|stop| (stop.id.clone(), stop.coordinate))
            .collect::<Vec<_>>();
        let destination_pool = self
            .network
            .nearest_stops(
                request.destination.coordinate,
                limits.max_destination_side_stops.max(1),
            )
            .into_iter()
            .filter(|stop| {
                geographic_distance_metres(request.destination.coordinate, stop.coordinate)
                    <= u64::from(limits.max_walking_radius_metres)
            })
            .map(|stop| (stop.id.clone(), stop.coordinate))
            .collect::<Vec<_>>();
        if origin_pool.is_empty() || destination_pool.is_empty() {
            return Ok(DiscoveryResult::default());
        }

        let origin_coordinates = origin_pool
            .iter()
            .map(|(_, coordinate)| planner_coordinate(*coordinate))
            .collect::<Result<Vec<_>, _>>()?;
        let destination_coordinates = destination_pool
            .iter()
            .map(|(_, coordinate)| planner_coordinate(*coordinate))
            .collect::<Result<Vec<_>, _>>()?;
        let access_matrix = self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::OneToMany,
                travel_mode: StreetTravelMode::Walking,
                sources: vec![origin],
                targets: origin_coordinates,
                constraints: context.access_constraints(),
            })
            .await?;
        let egress_matrix = self
            .street_routing
            .matrix(MatrixQuery {
                orientation: MatrixOrientation::ManyToOne,
                travel_mode: StreetTravelMode::Walking,
                sources: destination_coordinates,
                targets: vec![destination],
                constraints: context.direct_constraints(),
            })
            .await?;

        let mut origin_access = access_matrix
            .cells
            .first()
            .into_iter()
            .flatten()
            .filter_map(|cell| {
                let (stop_id, _) = origin_pool.get(cell.target_index)?;
                let metric = street_metric(cell)?;
                (metric.duration_seconds <= limits.max_walking_seconds).then(|| {
                    TransitAccessCandidate {
                        access: TransitAccess::new(
                            stop_id.clone(),
                            context.search_start,
                            context.search_start
                                + time::Duration::seconds(i64::from(metric.duration_seconds)),
                        ),
                        metric,
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut destination_egress = egress_matrix
            .cells
            .iter()
            .enumerate()
            .filter_map(|(source_index, cells)| {
                let (stop_id, _) = destination_pool.get(source_index)?;
                let metric = street_metric(cells.first()?)?;
                (metric.duration_seconds <= limits.max_walking_seconds).then(|| {
                    TransitEgressCandidate {
                        stop_id: stop_id.clone(),
                        metric,
                    }
                })
            })
            .collect::<Vec<_>>();

        origin_access.sort_by(|left, right| {
            left.metric
                .duration_seconds
                .cmp(&right.metric.duration_seconds)
                .then(
                    left.access
                        .stop_id
                        .as_str()
                        .cmp(right.access.stop_id.as_str()),
                )
        });
        destination_egress.sort_by(|left, right| {
            left.metric
                .duration_seconds
                .cmp(&right.metric.duration_seconds)
                .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
        });
        origin_access.truncate(limits.max_candidate_stops.max(1));
        destination_egress.truncate(limits.max_candidate_stops.max(1));
        if origin_access.is_empty() || destination_egress.is_empty() {
            return Ok(DiscoveryResult {
                origin_access,
                destination_egress,
                ..DiscoveryResult::default()
            });
        }

        let egress_ids = destination_egress
            .iter()
            .map(|egress| egress.stop_id.clone())
            .collect::<HashSet<_>>();
        let (services, path_edges) = self.relevant_services(&origin_access, &egress_ids, limits);
        let service_set = services.iter().cloned().collect::<HashSet<_>>();
        let service_day = self.network.service_day(context.effective_search_time);
        let topology_itineraries = self.topology_itineraries(
            (request.origin.coordinate, request.destination.coordinate),
            &origin_access,
            &egress_ids,
            &service_set,
            service_day,
            limits,
        );
        let mut path_edges = path_edges;
        for itinerary in &topology_itineraries {
            for pair in itinerary.rides.windows(2) {
                if pair[0].alight_stop_id == pair[1].board_stop_id
                    && pair[0].service_id != pair[1].service_id
                {
                    path_edges.push(TransferEdge::new(
                        pair[0].alight_stop_id.clone(),
                        pair[1].board_stop_id.clone(),
                        self.network.default_transfer_seconds,
                    ));
                }
            }
        }
        let mut boarding_points = origin_access
            .iter()
            .map(|access| access.access.stop_id.clone())
            .chain(
                destination_egress
                    .iter()
                    .map(|egress| egress.stop_id.clone()),
            )
            .collect::<Vec<_>>();
        boarding_points.extend(topology_itineraries.iter().flat_map(|itinerary| {
            itinerary
                .rides
                .iter()
                .flat_map(|ride| [ride.board_stop_id.clone(), ride.alight_stop_id.clone()])
        }));
        boarding_points.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        boarding_points.dedup();
        boarding_points.truncate(limits.max_candidate_stops.max(1));

        let mut transfer_edges = path_edges;
        transfer_edges.sort_by(|left, right| {
            left.from
                .as_str()
                .cmp(right.from.as_str())
                .then(left.to.as_str().cmp(right.to.as_str()))
                .then(left.duration_seconds.cmp(&right.duration_seconds))
        });
        transfer_edges.dedup();

        Ok(DiscoveryResult {
            boarding_points,
            services: service_set.into_iter().collect::<Vec<_>>(),
            transfer_edges,
            origin_access,
            destination_egress,
            topology_itineraries,
            direct_walking: None,
        }
        .sorted())
    }

    fn relevant_services(
        &self,
        origin_access: &[TransitAccessCandidate],
        egress_ids: &HashSet<BoardingPointId>,
        limits: &RoutingLimits,
    ) -> (Vec<TransitServiceId>, Vec<TransferEdge>) {
        let max_transfers = limits.max_transfer_count as usize;
        let max_states = limits.max_topology_search_states;
        let max_upstream_stops = limits.max_topology_stops_visited;
        let max_services_per_stop = limits.max_services_per_stop;
        let max_services = limits.max_candidate_services;
        #[derive(Clone)]
        struct State {
            stop_id: BoardingPointId,
            current_service: Option<TransitServiceId>,
            transfers: usize,
            stops_visited: usize,
            services: Vec<TransitServiceId>,
            edges: Vec<TransferEdge>,
        }

        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let origin_ids = origin_access
            .iter()
            .map(|access| access.access.stop_id.clone())
            .collect::<HashSet<_>>();
        let mut destination_ids = egress_ids.iter().cloned().collect::<Vec<_>>();
        destination_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        for stop_id in destination_ids.into_iter().take(max_states.max(1)) {
            queue.push_back(State {
                stop_id,
                current_service: None,
                transfers: 0,
                stops_visited: 0,
                services: Vec::new(),
                edges: Vec::new(),
            });
        }
        let mut relevant_services = Vec::new();
        let mut relevant_edges = Vec::new();
        let mut transfer_edges = self.network.transfer_edges.iter().collect::<Vec<_>>();
        transfer_edges.sort_by(|left, right| {
            left.from
                .as_str()
                .cmp(right.from.as_str())
                .then(left.to.as_str().cmp(right.to.as_str()))
                .then(left.duration_seconds.cmp(&right.duration_seconds))
        });
        let max_states = max_states.max(1);
        while let Some(state) = queue.pop_front() {
            if visited.len() >= max_states {
                break;
            }
            if !visited.insert((
                state.stop_id.clone(),
                state.current_service.clone(),
                state.transfers,
                state.services.clone(),
            )) {
                continue;
            }
            if origin_ids.contains(&state.stop_id) && !state.services.is_empty() {
                relevant_services.extend(state.services.iter().cloned());
                relevant_edges.extend(state.edges.iter().cloned());
            }
            if state.transfers > max_transfers || state.stops_visited >= max_upstream_stops {
                continue;
            }

            let mut services = self
                .network
                .services
                .iter()
                .filter(|service| {
                    service
                        .pattern
                        .iter()
                        .any(|stop_id| stop_id == &state.stop_id)
                })
                .collect::<Vec<_>>();
            services.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
            services.truncate(max_services_per_stop.max(1));
            for service in services {
                let Some(alight_index) = service
                    .pattern
                    .iter()
                    .position(|stop_id| stop_id == &state.stop_id)
                else {
                    continue;
                };
                for next_stop in service
                    .pattern
                    .iter()
                    .take(alight_index)
                    .rev()
                    .take(max_upstream_stops.max(1))
                {
                    if queue.len() >= max_states {
                        break;
                    }
                    let service_change = state
                        .current_service
                        .as_ref()
                        .is_some_and(|current| current != &service.id);
                    let transfers = state.transfers + usize::from(service_change);
                    if transfers > max_transfers {
                        continue;
                    }
                    let mut next_services = state.services.clone();
                    next_services.push(service.id.clone());
                    let next_state = State {
                        stop_id: next_stop.clone(),
                        current_service: Some(service.id.clone()),
                        transfers,
                        stops_visited: state.stops_visited.saturating_add(1),
                        services: next_services,
                        edges: state.edges.clone(),
                    };
                    queue.push_back(next_state.clone());
                }
            }
            // Reverse the physical transfer edges. Keeping the downstream
            // service in the state means a service change is counted only
            // when an upstream service is selected, not when a graph node is
            // crossed.
            for edge in transfer_edges
                .iter()
                .filter(|edge| edge.to == state.stop_id)
            {
                if queue.len() >= max_states {
                    break;
                }
                let mut transferred = state.clone();
                transferred.stop_id = edge.from.clone();
                transferred.edges.push((*edge).clone());
                transferred.stops_visited = transferred.stops_visited.saturating_add(1);
                queue.push_back(transferred);
            }
        }
        relevant_services.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        relevant_services.dedup();
        relevant_services.truncate(max_services.max(1));
        relevant_edges.sort_by(|left, right| {
            left.from
                .as_str()
                .cmp(right.from.as_str())
                .then(left.to.as_str().cmp(right.to.as_str()))
                .then(left.duration_seconds.cmp(&right.duration_seconds))
        });
        relevant_edges.dedup();
        (relevant_services, relevant_edges)
    }

    fn topology_itineraries(
        &self,
        geographic_bounds: (RequestCoordinate, RequestCoordinate),
        origin_access: &[TransitAccessCandidate],
        egress_ids: &HashSet<BoardingPointId>,
        service_ids: &HashSet<TransitServiceId>,
        service_day: time::Date,
        limits: &RoutingLimits,
    ) -> Vec<TransitTopologyItinerary> {
        #[derive(Clone)]
        struct State {
            stop_id: BoardingPointId,
            current_service: Option<TransitServiceId>,
            transfers: usize,
            stops_visited: usize,
            rides: Vec<TransitRide>,
        }

        let mut services = self.network.services.iter().collect::<Vec<_>>();
        services.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        let max_states = limits.max_topology_search_states.max(1);
        let mut queue = origin_access
            .iter()
            .take(max_states)
            .map(|access| State {
                stop_id: access.access.stop_id.clone(),
                current_service: None,
                transfers: 0,
                stops_visited: 0,
                rides: Vec::new(),
            })
            .collect::<VecDeque<_>>();
        let mut visited = HashSet::new();
        let mut results = Vec::new();
        while let Some(state) = queue.pop_front() {
            if visited.len() >= max_states || results.len() >= limits.max_transit_itineraries.max(1)
            {
                break;
            }
            let signature = state
                .rides
                .iter()
                .map(|ride| {
                    (
                        ride.service_id.clone(),
                        ride.board_stop_id.clone(),
                        ride.alight_stop_id.clone(),
                    )
                })
                .collect::<Vec<_>>();
            if !visited.insert((
                state.stop_id.clone(),
                state.current_service.clone(),
                state.transfers,
                signature,
            )) {
                continue;
            }
            if egress_ids.contains(&state.stop_id) && !state.rides.is_empty() {
                results.push(TransitTopologyItinerary {
                    service_day,
                    rides: state.rides.clone(),
                });
                continue;
            }
            if state.rides.len() > limits.max_transfer_count as usize + 1
                || state.stops_visited >= limits.max_topology_stops_visited.max(1)
            {
                continue;
            }
            for service in services.iter().filter(|service| {
                service_ids.contains(&service.id)
                    && service.pattern.iter().any(|id| id == &state.stop_id)
            }) {
                let Some(board_index) = service.pattern.iter().position(|id| id == &state.stop_id)
                else {
                    continue;
                };
                let service_change = state
                    .current_service
                    .as_ref()
                    .is_some_and(|current| current != &service.id);
                let transfers = state.transfers + usize::from(service_change);
                if transfers > limits.max_transfer_count as usize {
                    continue;
                }
                for (offset, alight_stop_id) in service
                    .pattern
                    .iter()
                    .skip(board_index + 1)
                    .take(limits.max_topology_stops_visited.max(1))
                    .enumerate()
                {
                    let stops_visited =
                        state.stops_visited.saturating_add(offset.saturating_add(1));
                    if stops_visited > limits.max_topology_stops_visited.max(1)
                        || visited.len().saturating_add(queue.len()) >= max_states
                    {
                        break;
                    }
                    let Some(ride) = self.network.topology_ride(
                        service_day,
                        &service.id,
                        &state.stop_id,
                        alight_stop_id,
                    ) else {
                        continue;
                    };
                    let mut next = state.clone();
                    next.stop_id = alight_stop_id.clone();
                    next.current_service = Some(service.id.clone());
                    next.transfers = transfers;
                    next.stops_visited = stops_visited;
                    next.rides.push(ride);
                    queue.push_back(next);
                }
            }
            for edge in self
                .network
                .transfer_edges
                .iter()
                .filter(|edge| edge.from == state.stop_id)
            {
                if visited.len().saturating_add(queue.len()) >= max_states {
                    break;
                }
                let mut next = state.clone();
                next.stop_id = edge.to.clone();
                next.stops_visited = next.stops_visited.saturating_add(1);
                queue.push_back(next);
            }
        }
        results.sort_by(|left, right| {
            topology_signature(&left.rides).cmp(&topology_signature(&right.rides))
        });
        results.dedup_by(|left, right| left.rides == right.rides);
        let direct_distance = geographic_distance_metres(geographic_bounds.0, geographic_bounds.1);
        let max_geographic_distance =
            direct_distance.saturating_add(u64::from(limits.max_geographic_detour_metres));
        results.retain(|itinerary| {
            topology_geographic_distance(self.network, &itinerary.rides) <= max_geographic_distance
        });
        results.truncate(limits.max_transit_itineraries.max(1));
        results
    }
}

impl DiscoveryResult {
    fn sorted(mut self) -> Self {
        self.services
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        self.services.dedup();
        self
    }
}

fn topology_signature(rides: &[TransitRide]) -> String {
    rides
        .iter()
        .map(|ride| {
            format!(
                "{}:{}:{}",
                ride.service_id.as_str(),
                ride.board_stop_id.as_str(),
                ride.alight_stop_id.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn geographic_distance_metres(from: RequestCoordinate, to: RequestCoordinate) -> u64 {
    let latitude = ((from.latitude + to.latitude) / 2.0).to_radians();
    let north = (to.latitude - from.latitude) * 111_320.0;
    let east = (to.longitude - from.longitude) * 111_320.0 * latitude.cos();
    north.hypot(east).max(0.0).round() as u64
}

fn topology_geographic_distance(network: &TransitNetwork, rides: &[TransitRide]) -> u64 {
    rides
        .iter()
        .filter_map(|ride| {
            Some(geographic_distance_metres(
                network.stop(&ride.board_stop_id)?.coordinate,
                network.stop(&ride.alight_stop_id)?.coordinate,
            ))
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::macros::datetime;

    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::ids::BoardingPointId,
        domain::transit::{TransitService, TransitStop},
        street::{FakeProvider, GraphRevision},
    };

    fn topology_network(service_count: usize) -> TransitNetwork {
        let stop = |id: &str, latitude| {
            TransitStop::new(
                BoardingPointId::new(id),
                id,
                RequestCoordinate {
                    latitude,
                    longitude: 121.5,
                },
            )
        };
        let a = BoardingPointId::new("a");
        let b = BoardingPointId::new("b");
        let c = BoardingPointId::new("c");
        let d = BoardingPointId::new("d");
        let mut services = Vec::new();
        for index in 0..service_count {
            services.push(TransitService::new(
                TransitServiceId::new(format!("service-{index:02}")),
                crate::domain::transit::TransitMode::Metro,
                format!("Service {index}"),
                "D",
                vec![a.clone(), d.clone()],
            ));
        }
        services.push(TransitService::new(
            TransitServiceId::new("connector-a"),
            crate::domain::transit::TransitMode::Metro,
            "Connector A",
            "B",
            vec![a.clone(), b.clone()],
        ));
        services.push(TransitService::new(
            TransitServiceId::new("connector-b"),
            crate::domain::transit::TransitMode::Metro,
            "Connector B",
            "D",
            vec![c.clone(), d],
        ));
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![
                stop("a", 25.0),
                stop("b", 25.01),
                stop("c", 25.02),
                stop("d", 25.03),
            ],
            services,
            Vec::new(),
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn topology_selection_is_bounded_and_deterministic() {
        let network = topology_network(20).with_transfer_edges(vec![TransferEdge::new(
            BoardingPointId::new("b"),
            BoardingPointId::new("c"),
            90,
        )]);
        let provider = FakeProvider::builder(GraphRevision::new("graph")).build();
        let street = StreetRoutingSession::start(Arc::new(provider))
            .await
            .unwrap();
        let discovery = Discovery::new(&street, &network);
        let access = vec![TransitAccessCandidate {
            access: TransitAccess::new(
                BoardingPointId::new("a"),
                datetime!(2026-08-31 10:00 +08:00),
                datetime!(2026-08-31 10:00 +08:00),
            ),
            metric: StreetMetric {
                duration_seconds: 60,
                distance_metres: 300,
            },
        }];
        let destination = HashSet::from([BoardingPointId::new("d")]);
        let limits = RoutingLimits {
            max_transfer_count: 1,
            max_candidate_states: 16,
            max_candidate_stops: 8,
            max_services_per_stop: 3,
            max_candidate_services: 5,
            ..RoutingLimits::default()
        };
        let first = discovery.relevant_services(&access, &destination, &limits);
        let second = discovery.relevant_services(&access, &destination, &limits);

        assert_eq!(first.0, second.0);
        assert_eq!(first.1, second.1);
        assert!(first.0.len() <= limits.max_candidate_services);
        assert!(first.1.len() <= limits.max_candidate_services);
        assert!(first.0.iter().all(|id| id.as_str() != "service-19"));
        assert_eq!(first.1[0].duration_seconds, 90);
    }
}
