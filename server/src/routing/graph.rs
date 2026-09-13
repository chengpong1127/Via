//! Common bounded route graph shared by walking, Transit, and BikeShare.

use std::collections::HashMap;

use crate::{
    domain::transit::{TransferEdge, TransitNetwork},
    domain::{
        ids::{BikeShareStationId, BoardingPointId},
        realtime::BikeAvailability,
    },
};

use super::{
    bike::BikeShareOpportunity,
    discover::DiscoveryResult,
    enrich::{TimedTransitRide, TransitTiming},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreetMetric {
    pub duration_seconds: u32,
    pub distance_metres: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RouteNode {
    Origin,
    Destination,
    TransitStop(BoardingPointId),
    BikeShareStation(BikeShareStationId),
}

/// A rider-visible walking segment in the base network. BikeShare expansion
/// treats this as the generic substitution seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkEdge {
    pub from: RouteNode,
    pub to: RouteNode,
    pub metric: StreetMetric,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeEdgeAvailability {
    pub pickup: BikeAvailability,
    pub return_station: BikeAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteEdge {
    Walk {
        from: RouteNode,
        to: RouteNode,
        metric: StreetMetric,
    },
    Transit {
        from: RouteNode,
        to: RouteNode,
        ride: TimedTransitRide,
    },
    Bike {
        from: RouteNode,
        to: RouteNode,
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        metric: StreetMetric,
        availability: BikeEdgeAvailability,
    },
}

impl RouteEdge {
    pub fn from(&self) -> &RouteNode {
        match self {
            Self::Walk { from, .. } | Self::Transit { from, .. } | Self::Bike { from, .. } => from,
        }
    }

    pub fn to(&self) -> &RouteNode {
        match self {
            Self::Walk { to, .. } | Self::Transit { to, .. } | Self::Bike { to, .. } => to,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RouteGraph {
    pub nodes: Vec<RouteNode>,
    pub edges: Vec<RouteEdge>,
    outgoing: HashMap<RouteNode, Vec<usize>>,
}

impl RouteGraph {
    pub fn outgoing(&self, node: &RouteNode) -> impl Iterator<Item = (usize, &RouteEdge)> {
        self.outgoing
            .get(node)
            .into_iter()
            .flatten()
            .map(|index| (*index, &self.edges[*index]))
    }

    pub fn add_edge(&mut self, edge: RouteEdge) {
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

    fn add_node(&mut self, node: RouteNode) {
        if !self.nodes.contains(&node) {
            self.nodes.push(node);
            self.nodes.sort_by_key(node_sort_key);
        }
    }

    pub fn bike_edges(&self) -> impl Iterator<Item = &RouteEdge> {
        self.edges
            .iter()
            .filter(|edge| matches!(edge, RouteEdge::Bike { .. }))
    }
}

pub fn add_timed_transit(graph: &mut RouteGraph, network: &TransitNetwork, timed: &TransitTiming) {
    for itinerary in &timed.itineraries {
        for (index, ride) in itinerary.rides.iter().enumerate() {
            graph.add_edge(RouteEdge::Transit {
                from: RouteNode::TransitStop(ride.ride.board_stop_id.clone()),
                to: RouteNode::TransitStop(ride.ride.alight_stop_id.clone()),
                ride: ride.clone(),
            });
            if let Some(next) = itinerary.rides.get(index + 1)
                && ride.ride.service_id != next.ride.service_id
                && ride.ride.alight_stop_id == next.ride.board_stop_id
            {
                graph.add_edge(RouteEdge::Walk {
                    from: RouteNode::TransitStop(ride.ride.alight_stop_id.clone()),
                    to: RouteNode::TransitStop(next.ride.board_stop_id.clone()),
                    metric: StreetMetric {
                        duration_seconds: network
                            .transfer_seconds(&ride.ride.alight_stop_id, &next.ride.board_stop_id)
                            .unwrap_or_default(),
                        distance_metres: 0,
                    },
                });
            }
        }
    }
}

pub fn add_bike_alternatives(
    graph: &mut RouteGraph,
    opportunities: &[BikeShareOpportunity],
    bike_availability: &HashMap<BikeShareStationId, BikeAvailability>,
) {
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
        graph.add_edge(RouteEdge::Walk {
            from: opportunity.segment.from.clone(),
            to: RouteNode::BikeShareStation(opportunity.pickup_station_id.clone()),
            metric: opportunity.access,
        });
        graph.add_edge(RouteEdge::Bike {
            from: RouteNode::BikeShareStation(opportunity.pickup_station_id.clone()),
            to: RouteNode::BikeShareStation(opportunity.return_station_id.clone()),
            pickup_station_id: opportunity.pickup_station_id.clone(),
            return_station_id: opportunity.return_station_id.clone(),
            metric: opportunity.bicycle,
            availability: BikeEdgeAvailability {
                pickup,
                return_station,
            },
        });
        graph.add_edge(RouteEdge::Walk {
            from: RouteNode::BikeShareStation(opportunity.return_station_id.clone()),
            to: opportunity.segment.to.clone(),
            metric: opportunity.egress,
        });
    }
}

/// Start the single routing graph with origin/destination access and untimed
/// topology edges discovered for this request.
pub fn from_discovery(candidate: &DiscoveryResult) -> RouteGraph {
    let mut graph = RouteGraph::default();
    graph.add_node(RouteNode::Origin);
    graph.add_node(RouteNode::Destination);
    if let Some(metric) = candidate.direct_walking {
        graph.add_edge(RouteEdge::Walk {
            from: RouteNode::Origin,
            to: RouteNode::Destination,
            metric,
        });
    }
    for access in &candidate.origin_access {
        graph.add_edge(RouteEdge::Walk {
            from: RouteNode::Origin,
            to: RouteNode::TransitStop(access.access.stop_id.clone()),
            metric: access.metric,
        });
    }
    for egress in &candidate.destination_egress {
        graph.add_edge(RouteEdge::Walk {
            from: RouteNode::TransitStop(egress.stop_id.clone()),
            to: RouteNode::Destination,
            metric: egress.metric,
        });
    }
    for edge in &candidate.transfer_edges {
        add_transfer(&mut graph, edge);
    }
    graph
}

fn add_transfer(graph: &mut RouteGraph, edge: &TransferEdge) {
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::TransitStop(edge.from.clone()),
        to: RouteNode::TransitStop(edge.to.clone()),
        metric: StreetMetric {
            duration_seconds: edge.duration_seconds,
            distance_metres: 0,
        },
    });
}

fn node_sort_key(node: &RouteNode) -> String {
    match node {
        RouteNode::Origin => "0:origin".into(),
        RouteNode::Destination => "1:destination".into(),
        RouteNode::TransitStop(stop_id) => format!("2:transit:{}", stop_id.as_str()),
        RouteNode::BikeShareStation(station_id) => format!("3:bike:{}", station_id.as_str()),
    }
}
