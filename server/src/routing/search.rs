//! Phase 5: bounded time-dependent multi-criteria search over the candidate
//! network.  Labels are compared by explicit Pareto dominance, never by one
//! weighted scalar cost.

use std::collections::{HashMap, VecDeque};

use time::{Duration, OffsetDateTime};

use super::graph::{RouteEdge, RouteGraph, RouteNode};
use crate::domain::{
    ids::TransitServiceId,
    realtime::{ActionState, FreshnessState},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchLimits {
    pub max_labels_per_node: usize,
    pub max_transfer_count: u32,
    pub max_bike_share_expansions: usize,
    pub max_search_expansions: usize,
    pub max_journey_duration_seconds: u32,
    pub max_final_pareto_journeys: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchLabel {
    pub node: RouteNode,
    pub arrival_time: OffsetDateTime,
    pub departure_time: OffsetDateTime,
    pub walking_seconds: u32,
    pub bike_share_seconds: u32,
    pub transfer_count: u32,
    pub bike_share_expansions: usize,
    pub required_freshness_states: Vec<FreshnessState>,
    pub schedule_only: bool,
    pub last_edge_was_transit: bool,
    pub edge_ids: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub expansions: usize,
    pub labels_retained: usize,
    pub labels_dominated_or_pruned: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    pub journeys: Vec<SearchLabel>,
    pub stats: SearchStats,
}

pub fn search(
    graph: &RouteGraph,
    departure_time: OffsetDateTime,
    latest_arrival: Option<OffsetDateTime>,
    limits: &SearchLimits,
) -> SearchResult {
    let start = SearchLabel {
        node: RouteNode::Origin,
        arrival_time: departure_time,
        departure_time,
        walking_seconds: 0,
        bike_share_seconds: 0,
        transfer_count: 0,
        bike_share_expansions: 0,
        required_freshness_states: Vec::new(),
        schedule_only: false,
        last_edge_was_transit: false,
        edge_ids: Vec::new(),
    };
    let mut labels: HashMap<RouteNode, Vec<SearchLabel>> = HashMap::new();
    labels.insert(RouteNode::Origin, vec![start.clone()]);
    let mut pending = VecDeque::from([start]);
    let mut stats = SearchStats::default();
    let mut journeys = Vec::new();

    while let Some(label) = pending.pop_front() {
        if stats.expansions >= limits.max_search_expansions {
            stats.labels_dominated_or_pruned = stats.labels_dominated_or_pruned.saturating_add(1);
            break;
        }
        stats.expansions += 1;
        for (edge_id, edge) in graph.outgoing(&label.node) {
            let Some(next) = traverse(graph, &label, edge, edge_id, limits) else {
                stats.labels_dominated_or_pruned =
                    stats.labels_dominated_or_pruned.saturating_add(1);
                continue;
            };
            if next.arrival_time - departure_time
                > Duration::seconds(i64::from(limits.max_journey_duration_seconds))
            {
                stats.labels_dominated_or_pruned =
                    stats.labels_dominated_or_pruned.saturating_add(1);
                continue;
            }
            if next.node == RouteNode::Destination {
                if latest_arrival.is_none_or(|latest| next.arrival_time <= latest) {
                    journeys.push(next.clone());
                }
                continue;
            }
            let inserted = insert_label(graph, &mut labels, next.clone(), limits, &mut stats);
            if inserted {
                pending.push_back(next);
            }
        }
    }

    let mut pareto = Vec::new();
    journeys.sort_by(label_order);
    for journey in journeys {
        if pareto.iter().any(|existing| dominates(existing, &journey)) {
            stats.labels_dominated_or_pruned = stats.labels_dominated_or_pruned.saturating_add(1);
            continue;
        }
        pareto.retain(|existing| !dominates(&journey, existing));
        if !pareto
            .iter()
            .any(|existing| existing.edge_ids == journey.edge_ids)
        {
            pareto.push(journey);
        }
    }
    pareto.sort_by(label_order);
    pareto.truncate(limits.max_final_pareto_journeys.max(1));
    stats.labels_retained = labels.values().map(Vec::len).sum::<usize>() + pareto.len();
    SearchResult {
        journeys: pareto,
        stats,
    }
}

pub fn dominates(left: &SearchLabel, right: &SearchLabel) -> bool {
    let no_worse = left.arrival_time <= right.arrival_time
        && left.walking_seconds <= right.walking_seconds
        && left.bike_share_seconds <= right.bike_share_seconds
        && left.transfer_count <= right.transfer_count
        && (!left.last_edge_was_transit || right.last_edge_was_transit)
        && (!left.schedule_only || right.schedule_only);
    let strictly_better = left.arrival_time < right.arrival_time
        || left.walking_seconds < right.walking_seconds
        || left.bike_share_seconds < right.bike_share_seconds
        || left.transfer_count < right.transfer_count
        || (left.last_edge_was_transit != right.last_edge_was_transit
            && !left.last_edge_was_transit)
        || (left.schedule_only != right.schedule_only && !left.schedule_only);
    no_worse && strictly_better
}

fn insert_label(
    graph: &RouteGraph,
    labels: &mut HashMap<RouteNode, Vec<SearchLabel>>,
    candidate: SearchLabel,
    limits: &SearchLimits,
    stats: &mut SearchStats,
) -> bool {
    let retained = labels.entry(candidate.node.clone()).or_default();
    if retained.iter().any(|existing| {
        existing.edge_ids == candidate.edge_ids
            || (same_transit_context(graph, existing, &candidate)
                && dominates(existing, &candidate))
    }) {
        stats.labels_dominated_or_pruned = stats.labels_dominated_or_pruned.saturating_add(1);
        return false;
    }
    let before = retained.len();
    retained.retain(|existing| {
        !same_transit_context(graph, &candidate, existing) || !dominates(&candidate, existing)
    });
    stats.labels_dominated_or_pruned += before.saturating_sub(retained.len());
    retained.push(candidate.clone());
    retained.sort_by(label_order);
    if retained.len() > limits.max_labels_per_node.max(1) {
        retained.truncate(limits.max_labels_per_node.max(1));
    }
    retained
        .iter()
        .any(|existing| existing.edge_ids == candidate.edge_ids)
}

fn traverse(
    graph: &RouteGraph,
    label: &SearchLabel,
    edge: &RouteEdge,
    edge_id: usize,
    limits: &SearchLimits,
) -> Option<SearchLabel> {
    let mut next = label.clone();
    next.node = edge.to().clone();
    next.edge_ids.push(edge_id);
    match edge {
        RouteEdge::Walk { metric, .. } => {
            next.last_edge_was_transit = false;
            next.arrival_time += Duration::seconds(i64::from(metric.duration_seconds));
            next.walking_seconds = next.walking_seconds.saturating_add(metric.duration_seconds);
        }
        RouteEdge::Bike {
            metric,
            availability,
            ..
        } => {
            next.last_edge_was_transit = false;
            if availability.pickup.rental_state != ActionState::Enabled
                || availability.return_station.return_state != ActionState::Enabled
                || availability
                    .pickup
                    .available_bikes
                    .is_none_or(|count| count == 0)
                || availability
                    .return_station
                    .available_return_slots
                    .is_none_or(|count| count == 0)
            {
                return None;
            }
            next.arrival_time += Duration::seconds(i64::from(metric.duration_seconds));
            next.bike_share_seconds = next
                .bike_share_seconds
                .saturating_add(metric.duration_seconds);
            next.bike_share_expansions += 1;
            if next.bike_share_expansions > limits.max_bike_share_expansions {
                return None;
            }
        }
        RouteEdge::Transit { ride, .. } => {
            let previous_service = previous_transit_service(graph, label);
            if label.last_edge_was_transit
                && previous_service.is_some_and(|service| service != &ride.ride.service_id)
            {
                // A different service requires a physical connector. A
                // same-service continuation remains one passenger-visible leg.
                return None;
            }
            if previous_service.is_some_and(|service| service != &ride.ride.service_id) {
                next.transfer_count = next.transfer_count.saturating_add(1);
                if next.transfer_count > limits.max_transfer_count {
                    return None;
                }
            }
            if label.arrival_time > ride.departure {
                return None;
            }
            next.last_edge_was_transit = true;
            next.arrival_time = ride.arrival;
            next.required_freshness_states
                .extend(ride.required_states.iter().copied());
            next.schedule_only |= ride.schedule_only;
        }
    }
    Some(next)
}

fn previous_transit_service<'a>(
    graph: &'a RouteGraph,
    label: &SearchLabel,
) -> Option<&'a TransitServiceId> {
    label.edge_ids.iter().rev().find_map(|edge_id| {
        let RouteEdge::Transit { ride, .. } = graph.edges.get(*edge_id)? else {
            return None;
        };
        Some(&ride.ride.service_id)
    })
}

fn same_transit_context(graph: &RouteGraph, left: &SearchLabel, right: &SearchLabel) -> bool {
    previous_transit_service(graph, left) == previous_transit_service(graph, right)
}

fn label_order(left: &SearchLabel, right: &SearchLabel) -> std::cmp::Ordering {
    left.arrival_time
        .cmp(&right.arrival_time)
        .then(left.walking_seconds.cmp(&right.walking_seconds))
        .then(left.transfer_count.cmp(&right.transfer_count))
        .then(left.bike_share_seconds.cmp(&right.bike_share_seconds))
        .then(left.edge_ids.cmp(&right.edge_ids))
}
