//! Common bounded candidate graph shared by walking, Transit, and BikeShare.

use std::collections::HashMap;

use crate::{
    normalization::{BikeAvailability, BikeShareStationId, BoardingPointId},
    transit::{TransferEdge, TransitNetwork},
};

use super::{
    StreetMetric,
    bike_share_expansion::BikeShareOpportunity,
    candidate_discovery::TransitCandidateNetwork,
    transit_timing::{TimedTransitBackbone, TimedTransitRide},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CandidateNode {
    Origin,
    Destination,
    TransitStop(BoardingPointId),
    BikeShareStation(BikeShareStationId),
}

/// Provider-neutral names used by the routing boundary. The older candidate
/// names remain available to avoid breaking downstream integrations.
pub type Node = CandidateNode;
pub type Edge = CandidateEdge;
pub type BaseNetwork = MultimodalGraph;

/// A rider-visible walking segment in the base network. BikeShare expansion
/// treats this as the generic substitution seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkingEdge {
    pub from: CandidateNode,
    pub to: CandidateNode,
    pub metric: StreetMetric,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareAvailability {
    pub pickup: BikeAvailability,
    pub return_station: BikeAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateEdge {
    Walking {
        from: CandidateNode,
        to: CandidateNode,
        metric: StreetMetric,
    },
    Transit {
        from: CandidateNode,
        to: CandidateNode,
        ride: TimedTransitRide,
    },
    BikeShare {
        from: CandidateNode,
        to: CandidateNode,
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        metric: StreetMetric,
        availability: BikeShareAvailability,
    },
    Transfer {
        from: CandidateNode,
        to: CandidateNode,
        duration_seconds: u32,
    },
}

impl CandidateEdge {
    pub fn from(&self) -> &CandidateNode {
        match self {
            Self::Walking { from, .. }
            | Self::Transit { from, .. }
            | Self::BikeShare { from, .. }
            | Self::Transfer { from, .. } => from,
        }
    }

    pub fn to(&self) -> &CandidateNode {
        match self {
            Self::Walking { to, .. }
            | Self::Transit { to, .. }
            | Self::BikeShare { to, .. }
            | Self::Transfer { to, .. } => to,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MultimodalGraph {
    pub nodes: Vec<CandidateNode>,
    pub edges: Vec<CandidateEdge>,
    outgoing: HashMap<CandidateNode, Vec<usize>>,
}

impl MultimodalGraph {
    pub fn outgoing(&self, node: &CandidateNode) -> impl Iterator<Item = (usize, &CandidateEdge)> {
        self.outgoing
            .get(node)
            .into_iter()
            .flatten()
            .map(|index| (*index, &self.edges[*index]))
    }

    pub fn add_edge(&mut self, edge: CandidateEdge) {
        if self.edges.iter().any(|existing| existing == &edge) {
            return;
        }
        let from = edge.from().clone();
        let to = edge.to().clone();
        self.add_node(from.clone());
        self.add_node(to);
        let index = self.edges.len();
        self.edges.push(edge);
        self.outgoing.entry(from).or_default().push(index);
    }

    fn add_node(&mut self, node: CandidateNode) {
        if !self.nodes.contains(&node) {
            self.nodes.push(node);
            self.nodes.sort_by_key(node_sort_key);
        }
    }

    pub fn bike_edges(&self) -> impl Iterator<Item = &CandidateEdge> {
        self.edges
            .iter()
            .filter(|edge| matches!(edge, CandidateEdge::BikeShare { .. }))
    }
}

pub fn build_graph(
    network: &TransitNetwork,
    candidate: &TransitCandidateNetwork,
    timed: &TimedTransitBackbone,
    opportunities: &[BikeShareOpportunity],
    bike_availability: &HashMap<BikeShareStationId, BikeAvailability>,
) -> MultimodalGraph {
    let mut graph = build_base_graph(candidate);

    for itinerary in &timed.itineraries {
        for (index, ride) in itinerary.rides.iter().enumerate() {
            graph.add_edge(CandidateEdge::Transit {
                from: CandidateNode::TransitStop(ride.ride.board_stop_id.clone()),
                to: CandidateNode::TransitStop(ride.ride.alight_stop_id.clone()),
                ride: ride.clone(),
            });
            if let Some(next) = itinerary.rides.get(index + 1)
                && ride.ride.service_id != next.ride.service_id
                && ride.ride.alight_stop_id == next.ride.board_stop_id
            {
                graph.add_edge(CandidateEdge::Transfer {
                    from: CandidateNode::TransitStop(ride.ride.alight_stop_id.clone()),
                    to: CandidateNode::TransitStop(next.ride.board_stop_id.clone()),
                    duration_seconds: network
                        .transfer_seconds(&ride.ride.alight_stop_id, &next.ride.board_stop_id)
                        .unwrap_or_default(),
                });
            }
        }
    }

    for opportunity in opportunities {
        let Some(pickup) = bike_availability
            .get(&opportunity.pickup_station_id)
            .cloned()
        else {
            continue;
        };
        let Some(return_station) = bike_availability
            .get(&opportunity.return_station_id)
            .cloned()
        else {
            continue;
        };
        graph.add_edge(CandidateEdge::Walking {
            from: opportunity.walking_edge.from.clone(),
            to: CandidateNode::BikeShareStation(opportunity.pickup_station_id.clone()),
            metric: opportunity.access,
        });
        graph.add_edge(CandidateEdge::BikeShare {
            from: CandidateNode::BikeShareStation(opportunity.pickup_station_id.clone()),
            to: CandidateNode::BikeShareStation(opportunity.return_station_id.clone()),
            pickup_station_id: opportunity.pickup_station_id.clone(),
            return_station_id: opportunity.return_station_id.clone(),
            metric: opportunity.bicycle,
            availability: BikeShareAvailability {
                pickup,
                return_station,
            },
        });
        graph.add_edge(CandidateEdge::Walking {
            from: CandidateNode::BikeShareStation(opportunity.return_station_id.clone()),
            to: opportunity.walking_edge.to.clone(),
            metric: opportunity.egress,
        });
    }
    graph
}

/// Build the Walking + transfer base graph before any timed transit or
/// BikeShare edges are added.
pub fn build_base_graph(candidate: &TransitCandidateNetwork) -> MultimodalGraph {
    let mut graph = MultimodalGraph::default();
    graph.add_node(CandidateNode::Origin);
    graph.add_node(CandidateNode::Destination);
    if let Some(metric) = candidate.direct_walking {
        graph.add_edge(CandidateEdge::Walking {
            from: CandidateNode::Origin,
            to: CandidateNode::Destination,
            metric,
        });
    }
    for access in &candidate.origin_access {
        graph.add_edge(CandidateEdge::Walking {
            from: CandidateNode::Origin,
            to: CandidateNode::TransitStop(access.access.stop_id.clone()),
            metric: access.metric,
        });
    }
    for egress in &candidate.destination_egress {
        graph.add_edge(CandidateEdge::Walking {
            from: CandidateNode::TransitStop(egress.stop_id.clone()),
            to: CandidateNode::Destination,
            metric: egress.metric,
        });
    }
    for edge in &candidate.transfer_edges {
        add_transfer(&mut graph, edge);
    }
    graph
}

fn add_transfer(graph: &mut MultimodalGraph, edge: &TransferEdge) {
    graph.add_edge(CandidateEdge::Transfer {
        from: CandidateNode::TransitStop(edge.from.clone()),
        to: CandidateNode::TransitStop(edge.to.clone()),
        duration_seconds: edge.duration_seconds,
    });
}

fn node_sort_key(node: &CandidateNode) -> String {
    match node {
        CandidateNode::Origin => "0:origin".into(),
        CandidateNode::Destination => "1:destination".into(),
        CandidateNode::TransitStop(stop_id) => format!("2:transit:{}", stop_id.as_str()),
        CandidateNode::BikeShareStation(station_id) => format!("3:bike:{}", station_id.as_str()),
    }
}
