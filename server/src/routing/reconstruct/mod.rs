//! Reconstruction of surviving graph paths into canonical journey results.

mod evidence;
mod geometry;
mod legs;

use std::collections::HashMap;

use time::{Duration, OffsetDateTime};

use crate::{
    api::{PlanningRequest, normalize_to_taipei},
    domain::{
        journey::{JourneyOption, Place, PlanningError},
        ranking::RankedJourney,
    },
};

use super::{
    enrich, graph, search,
    service::{GeneratedCandidate, PlanningContext, RoutingService},
};

pub(crate) use geometry::{taipei_now, to_street_coordinate};

impl RoutingService {
    pub(crate) async fn reconstruct_multimodal_candidate(
        &self,
        request: &PlanningRequest,
        graph: &graph::RouteGraph,
        journey: &search::SearchLabel,
        realtime: &enrich::RealtimeContext,
        context: &PlanningContext,
        direct_walking_path: Option<&crate::street::DetailedPath>,
    ) -> Result<Option<GeneratedCandidate>, PlanningError> {
        let departure_time = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { time } => {
                let deadline = normalize_to_taipei(time);
                let Some(departure) = latest_departure_for_path(graph, &journey.edge_ids, deadline)
                else {
                    return Ok(None);
                };
                departure
            }
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => journey.departure_time,
        };

        let Some(assembled) = legs::assemble(
            self,
            request,
            graph,
            journey,
            realtime,
            context,
            direct_walking_path,
            departure_time,
        )
        .await?
        else {
            return Ok(None);
        };
        if !matches!(journey.node, graph::RouteNode::Destination) {
            return Ok(None);
        }

        let Some(duration_seconds) = legs::seconds_between(departure_time, assembled.arrival_time)
        else {
            return Ok(None);
        };
        if matches!(
            request.departure_setting,
            crate::api::DepartureSetting::ArriveBy { .. }
        ) && assembled.arrival_time
            > request
                .departure_setting
                .effective_search_time(taipei_now())
        {
            return Ok(None);
        }

        let matching_key = geometry::short_digest(&format!(
            "multimodal:v1:{}:{}",
            assembled.main_service_sequence.join(","),
            journey
                .edge_ids
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key: matching_key.clone(),
            class: crate::domain::journey::JourneyClass::Reliable,
            label: None,
            ranking_reasons: Vec::new(),
            departure_time,
            arrival_time: assembled.arrival_time,
            duration_seconds,
            walking_seconds: assembled.walking_seconds,
            transfer_count: journey.transfer_count,
            travel_modes: assembled.travel_modes,
            live_status_summary: evidence::summary_from_evidence(&assembled.live_evidence),
            overview_geometry: geometry::overview_geometry(&assembled.legs),
            legs: assembled.legs,
            transfers: assembled.transfers,
            warnings: Vec::new(),
        };
        Ok(Some(GeneratedCandidate {
            ranking: crate::domain::ranking::CandidateJourney {
                stable_key: format!("{}:{}", matching_key, departure_time.unix_timestamp()),
                departure_epoch_seconds: departure_time.unix_timestamp(),
                arrival_epoch_seconds: assembled.arrival_time.unix_timestamp(),
                walking_seconds: assembled.walking_seconds,
                transfer_count: journey.transfer_count,
                main_service_sequence: assembled.main_service_sequence,
                required_live_status: assembled.required_states,
                transfers: assembled.transfer_evidence,
                bike_availability: assembled.bike_availability,
                blocking_condition: false,
            },
            option,
            schedule_only: assembled.schedule_only,
        }))
    }

    pub(crate) fn node_place(
        &self,
        request: &PlanningRequest,
        node: &graph::RouteNode,
    ) -> Option<Place> {
        match node {
            graph::RouteNode::Origin => Some(Place {
                display_name: request.origin.display_text.clone(),
                coordinate: request.origin.coordinate,
                kind: "selected_place",
                via_id: None,
            }),
            graph::RouteNode::Destination => Some(Place {
                display_name: request.destination.display_text.clone(),
                coordinate: request.destination.coordinate,
                kind: "selected_place",
                via_id: None,
            }),
            graph::RouteNode::TransitStop(stop_id) => {
                self.transit_network.stop(stop_id).map(legs::transit_place)
            }
            graph::RouteNode::BikeShareStation(station_id) => self
                .transit_network
                .bike_share_station(station_id)
                .map(legs::bike_place),
        }
    }
}

pub(crate) fn latest_departure_for_path(
    graph: &graph::RouteGraph,
    edge_ids: &[usize],
    latest_arrival: OffsetDateTime,
) -> Option<OffsetDateTime> {
    let mut required_arrival = latest_arrival;
    for edge_id in edge_ids.iter().rev() {
        let edge = graph.edges.get(*edge_id)?;
        match edge {
            graph::RouteEdge::Walk { metric, .. } | graph::RouteEdge::Bike { metric, .. } => {
                required_arrival -= Duration::seconds(i64::from(metric.duration_seconds));
            }
            graph::RouteEdge::Transit { ride, .. } => {
                if ride.arrival > required_arrival {
                    return None;
                }
                required_arrival = ride.departure;
            }
        }
    }
    Some(required_arrival)
}

pub(crate) fn apply_ranked_journey(option: &mut JourneyOption, ranked: &RankedJourney) {
    option.class = ranked.class;
    option.label = ranked.label;
    option.ranking_reasons = ranked.ranking_reasons.clone();
}

pub(crate) fn assign_result_local_ids(option: &mut JourneyOption, index: usize) {
    let suffix = geometry::short_digest(&format!("{}:{index}", option.matching_key));
    option.id = format!("option_{:02}_{}", index + 1, suffix);
    let mut ids = HashMap::new();
    for (leg_index, leg) in option.legs.iter_mut().enumerate() {
        let previous = leg.id.clone();
        leg.id = format!("leg_{:02}_{}_{suffix}", index + 1, leg_index + 1);
        ids.insert(previous, leg.id.clone());
    }
    for transfer in &mut option.transfers {
        if let Some(id) = ids.get(&transfer.from_leg_id) {
            transfer.from_leg_id = id.clone();
        }
        if let Some(id) = ids.get(&transfer.to_leg_id) {
            transfer.to_leg_id = id.clone();
        }
        transfer.connector_leg_ids = transfer
            .connector_leg_ids
            .iter()
            .filter_map(|id| ids.get(id).cloned())
            .collect();
    }
}
