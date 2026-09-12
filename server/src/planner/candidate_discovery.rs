//! Phase 1: find the small, deterministic transit backbone relevant to one
//! Planning Request.  This module deliberately knows nothing about BikeShare
//! stations or realtime values.

use std::collections::{HashSet, VecDeque};

use crate::{
    api::PlanningRequest,
    normalization::TransitServiceId,
    planner::{PlanningContext, PlanningLimits, StreetMetric, planner_coordinate, street_metric},
    street::{
        MatrixOrientation, MatrixQuery, ProviderError, StreetRoutingSession, StreetTravelMode,
    },
    transit::{TransferEdge, TransitAccess, TransitNetwork},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitAccessCandidate {
    pub access: TransitAccess,
    pub metric: StreetMetric,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitEgressCandidate {
    pub stop_id: crate::normalization::BoardingPointId,
    pub metric: StreetMetric,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitCandidateNetwork {
    pub boarding_points: Vec<crate::normalization::BoardingPointId>,
    pub services: Vec<TransitServiceId>,
    pub transfer_edges: Vec<TransferEdge>,
    pub origin_access: Vec<TransitAccessCandidate>,
    pub destination_egress: Vec<TransitEgressCandidate>,
}

impl TransitCandidateNetwork {
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
            || self.origin_access.is_empty()
            || self.destination_egress.is_empty()
    }

    pub fn access_for(
        &self,
        stop_id: &crate::normalization::BoardingPointId,
    ) -> Option<&TransitAccessCandidate> {
        self.origin_access
            .iter()
            .find(|access| &access.access.stop_id == stop_id)
    }

    pub fn egress_for(
        &self,
        stop_id: &crate::normalization::BoardingPointId,
    ) -> Option<&TransitEgressCandidate> {
        self.destination_egress
            .iter()
            .find(|egress| &egress.stop_id == stop_id)
    }
}

#[derive(Clone)]
pub struct TransitCandidateDiscovery<'a> {
    street_routing: &'a StreetRoutingSession,
    network: &'a TransitNetwork,
}

impl<'a> TransitCandidateDiscovery<'a> {
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
    ) -> Result<TransitCandidateNetwork, ProviderError> {
        if !request
            .selected_modes
            .contains(&crate::api::JourneyMode::Transit)
        {
            return Ok(TransitCandidateNetwork::default());
        }
        let origin = planner_coordinate(request.origin.coordinate)?;
        let destination = planner_coordinate(request.destination.coordinate)?;

        let origin_pool = self
            .network
            .nearest_stops(request.origin.coordinate, limits.max_candidate_stops.max(1))
            .into_iter()
            .map(|stop| (stop.id.clone(), stop.coordinate))
            .collect::<Vec<_>>();
        let destination_pool = self
            .network
            .nearest_stops(
                request.destination.coordinate,
                limits.max_candidate_stops.max(1),
            )
            .into_iter()
            .map(|stop| (stop.id.clone(), stop.coordinate))
            .collect::<Vec<_>>();
        if origin_pool.is_empty() || destination_pool.is_empty() {
            return Ok(TransitCandidateNetwork::default());
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
            return Ok(TransitCandidateNetwork {
                origin_access,
                destination_egress,
                ..TransitCandidateNetwork::default()
            });
        }

        let egress_ids = destination_egress
            .iter()
            .map(|egress| egress.stop_id.clone())
            .collect::<HashSet<_>>();
        let (services, path_edges) = self.relevant_services(&origin_access, &egress_ids, limits);
        let service_set = services.iter().cloned().collect::<HashSet<_>>();
        let mut boarding_points = origin_access
            .iter()
            .map(|access| access.access.stop_id.clone())
            .chain(
                destination_egress
                    .iter()
                    .map(|egress| egress.stop_id.clone()),
            )
            .collect::<Vec<_>>();
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

        Ok(TransitCandidateNetwork {
            boarding_points,
            services: service_set.into_iter().collect::<Vec<_>>(),
            transfer_edges,
            origin_access,
            destination_egress,
        }
        .sorted())
    }

    fn relevant_services(
        &self,
        origin_access: &[TransitAccessCandidate],
        egress_ids: &HashSet<crate::normalization::BoardingPointId>,
        limits: &PlanningLimits,
    ) -> (Vec<TransitServiceId>, Vec<TransferEdge>) {
        let max_transfers = limits.max_transfer_count as usize;
        let max_states = limits.max_candidate_states;
        let max_downstream_stops = limits.max_candidate_stops;
        let max_services_per_stop = limits.max_services_per_stop;
        let max_services = limits.max_candidate_services;
        #[derive(Clone)]
        struct State {
            stop_id: crate::normalization::BoardingPointId,
            transfers: usize,
            services: Vec<TransitServiceId>,
            edges: Vec<TransferEdge>,
        }

        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        for access in origin_access {
            queue.push_back(State {
                stop_id: access.access.stop_id.clone(),
                transfers: 0,
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
            if !visited.insert((state.stop_id.clone(), state.transfers)) {
                continue;
            }
            if egress_ids.contains(&state.stop_id) && !state.services.is_empty() {
                relevant_services.extend(state.services.iter().cloned());
                relevant_edges.extend(state.edges.iter().cloned());
                if relevant_services.len() >= max_services {
                    break;
                }
            }
            if state.transfers > max_transfers {
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
                let Some(board_index) = service
                    .pattern
                    .iter()
                    .position(|stop_id| stop_id == &state.stop_id)
                else {
                    continue;
                };
                for next_stop in service
                    .pattern
                    .iter()
                    .skip(board_index + 1)
                    .take(max_downstream_stops.max(1))
                {
                    if queue.len() >= max_states {
                        break;
                    }
                    let mut next_services = state.services.clone();
                    next_services.push(service.id.clone());
                    next_services.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                    next_services.dedup();
                    let next_state = State {
                        stop_id: next_stop.clone(),
                        transfers: state.transfers,
                        services: next_services,
                        edges: state.edges.clone(),
                    };
                    if egress_ids.contains(next_stop) {
                        relevant_services.extend(next_state.services.iter().cloned());
                        relevant_edges.extend(next_state.edges.iter().cloned());
                        if relevant_services.len() >= max_services {
                            break;
                        }
                    }
                    queue.push_back(next_state.clone());
                    if state.transfers < max_transfers {
                        for edge in transfer_edges.iter().filter(|edge| &edge.from == next_stop) {
                            if queue.len() >= max_states {
                                break;
                            }
                            let mut transferred = next_state.clone();
                            transferred.stop_id = edge.to.clone();
                            transferred.transfers += 1;
                            transferred.edges.push((*edge).clone());
                            queue.push_back(transferred);
                        }
                    }
                }
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
}

impl TransitCandidateNetwork {
    fn sorted(mut self) -> Self {
        self.services
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        self.services.dedup();
        self
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::macros::datetime;

    use super::*;
    use crate::{
        api::RequestCoordinate,
        normalization::BoardingPointId,
        street::{FakeProvider, GraphRevision},
        transit::{TransitService, TransitStop},
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
                crate::transit::TransitMode::Metro,
                format!("Service {index}"),
                "D",
                vec![a.clone(), d.clone()],
            ));
        }
        services.push(TransitService::new(
            TransitServiceId::new("connector-a"),
            crate::transit::TransitMode::Metro,
            "Connector A",
            "B",
            vec![a.clone(), b.clone()],
        ));
        services.push(TransitService::new(
            TransitServiceId::new("connector-b"),
            crate::transit::TransitMode::Metro,
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
        let discovery = TransitCandidateDiscovery::new(&street, &network);
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
        let limits = PlanningLimits {
            max_transfer_count: 1,
            max_candidate_states: 16,
            max_candidate_stops: 8,
            max_services_per_stop: 3,
            max_candidate_services: 5,
            ..PlanningLimits::default()
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
