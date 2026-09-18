//! Reconstruct algorithm-neutral candidates into API journey options.

mod evidence;
mod geometry;
mod legs;

use std::collections::HashMap;

use crate::{
    api::{PlanningRequest, normalize_to_taipei},
    domain::{
        journey::{JourneyClass, JourneyMode, JourneyOption, Place, PlanningError},
        ranking::RankedJourney,
    },
};

use super::{
    algorithm::JourneyCandidate,
    bike::MobilityPath,
    service::{GeneratedCandidate, PlanningContext, RoutingService},
    status::RealtimeContext,
};

pub(crate) use geometry::{taipei_now, to_street_coordinate};

impl RoutingService {
    pub(crate) async fn reconstruct_multimodal_candidate(
        &self,
        request: &PlanningRequest,
        candidate: &JourneyCandidate,
        realtime: &RealtimeContext,
        context: &PlanningContext,
    ) -> Result<Option<GeneratedCandidate>, PlanningError> {
        let Some(assembled) = legs::assemble(self, request, candidate, realtime, context).await?
        else {
            return Ok(None);
        };
        if context
            .latest_arrival
            .is_some_and(|latest| assembled.arrival_time > latest)
        {
            return Ok(None);
        }
        let duration_seconds =
            legs::seconds_between(candidate.departure_time, assembled.arrival_time)
                .ok_or(PlanningError::Internal)?;
        let matching_key =
            geometry::short_digest(&format!("candidate:v2:{}", candidate.signature()));
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key: matching_key.clone(),
            class: JourneyClass::Reliable,
            label: None,
            ranking_reasons: Vec::new(),
            departure_time: candidate.departure_time,
            arrival_time: assembled.arrival_time,
            duration_seconds,
            walking_seconds: assembled.walking_seconds,
            transfer_count: candidate.transfer_count,
            travel_modes: assembled.travel_modes,
            live_status_summary: evidence::summary_from_evidence(&assembled.live_evidence),
            overview_geometry: geometry::overview_geometry(&assembled.legs),
            legs: assembled.legs,
            transfers: assembled.transfers,
            warnings: Vec::new(),
        };
        Ok(Some(GeneratedCandidate {
            ranking: crate::domain::ranking::CandidateJourney {
                stable_key: format!(
                    "{}:{}",
                    matching_key,
                    candidate.departure_time.unix_timestamp()
                ),
                departure_epoch_seconds: candidate.departure_time.unix_timestamp(),
                arrival_epoch_seconds: assembled.arrival_time.unix_timestamp(),
                walking_seconds: assembled.walking_seconds,
                transfer_count: candidate.transfer_count,
                bike_share_seconds: candidate.bike_seconds,
                main_service_sequence: assembled.main_service_sequence,
                timing_bases: candidate.timing_bases.clone(),
                required_live_status: assembled.required_states,
                transfers: assembled.transfer_evidence,
                bike_availability: assembled.bike_availability,
                blocking_condition: false,
            },
            option,
        }))
    }

    pub(crate) async fn reconstruct_direct_walking_candidate(
        &self,
        request: &PlanningRequest,
        context: &PlanningContext,
        path: &crate::street::StreetPath,
    ) -> Result<Option<GeneratedCandidate>, PlanningError> {
        let arrival = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { time } => normalize_to_taipei(time),
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => {
                context.search_start + time::Duration::seconds(i64::from(path.duration.0))
            }
        };
        let departure = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { .. } => {
                arrival - time::Duration::seconds(i64::from(path.duration.0))
            }
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => context.search_start,
        };
        let leg = legs::direct_walking_leg(self, request, path, departure, arrival)?;
        let matching_key = geometry::short_digest("direct-walking:v1");
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key: matching_key.clone(),
            class: JourneyClass::Reliable,
            label: None,
            ranking_reasons: Vec::new(),
            departure_time: departure,
            arrival_time: arrival,
            duration_seconds: path.duration.0,
            walking_seconds: path.duration.0,
            transfer_count: 0,
            travel_modes: vec![JourneyMode::Walking],
            live_status_summary: evidence::summary_from_evidence(&[]),
            overview_geometry: geometry::overview_geometry(std::slice::from_ref(&leg)),
            legs: vec![leg],
            transfers: Vec::new(),
            warnings: Vec::new(),
        };
        Ok(Some(GeneratedCandidate {
            ranking: crate::domain::ranking::CandidateJourney {
                stable_key: format!("{}:{}", matching_key, departure.unix_timestamp()),
                departure_epoch_seconds: departure.unix_timestamp(),
                arrival_epoch_seconds: arrival.unix_timestamp(),
                walking_seconds: path.duration.0,
                transfer_count: 0,
                bike_share_seconds: 0,
                main_service_sequence: Vec::new(),
                timing_bases: Vec::new(),
                required_live_status: Vec::new(),
                transfers: Vec::new(),
                bike_availability: None,
                blocking_condition: false,
            },
            option,
        }))
    }

    pub(crate) async fn reconstruct_direct_bike_candidate(
        &self,
        request: &PlanningRequest,
        path: &MobilityPath,
        realtime: &RealtimeContext,
        context: &PlanningContext,
    ) -> Result<Option<GeneratedCandidate>, PlanningError> {
        let departure = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { .. } => {
                context.latest_arrival.ok_or(PlanningError::Internal)?
                    - time::Duration::seconds(i64::from(path.duration_seconds()))
            }
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => context.search_start,
        };
        let Some(assembled) =
            legs::assemble_direct_mobility(self, request, path, departure, realtime, context)
                .await?
        else {
            return Ok(None);
        };
        if context
            .latest_arrival
            .is_some_and(|latest| assembled.arrival_time > latest)
        {
            return Ok(None);
        }
        let duration_seconds = legs::seconds_between(departure, assembled.arrival_time)
            .ok_or(PlanningError::Internal)?;
        let matching_key = geometry::short_digest(&format!(
            "direct-bike:v1:{}",
            path.required_station_ids()
                .iter()
                .map(|station| station.as_str())
                .collect::<Vec<_>>()
                .join(":")
        ));
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key: matching_key.clone(),
            class: JourneyClass::Reliable,
            label: None,
            ranking_reasons: Vec::new(),
            departure_time: departure,
            arrival_time: assembled.arrival_time,
            duration_seconds,
            walking_seconds: assembled.walking_seconds,
            transfer_count: 0,
            travel_modes: assembled.travel_modes,
            live_status_summary: evidence::summary_from_evidence(&assembled.live_evidence),
            overview_geometry: geometry::overview_geometry(&assembled.legs),
            legs: assembled.legs,
            transfers: assembled.transfers,
            warnings: Vec::new(),
        };
        Ok(Some(GeneratedCandidate {
            ranking: crate::domain::ranking::CandidateJourney {
                stable_key: format!("{}:{}", matching_key, departure.unix_timestamp()),
                departure_epoch_seconds: departure.unix_timestamp(),
                arrival_epoch_seconds: assembled.arrival_time.unix_timestamp(),
                walking_seconds: assembled.walking_seconds,
                transfer_count: 0,
                bike_share_seconds: path.bike_seconds(),
                main_service_sequence: assembled.main_service_sequence,
                timing_bases: Vec::new(),
                required_live_status: assembled.required_states,
                transfers: assembled.transfer_evidence,
                bike_availability: assembled.bike_availability,
                blocking_condition: false,
            },
            option,
        }))
    }

    pub(crate) fn origin_place(&self, request: &PlanningRequest) -> Place {
        Place {
            display_name: request.origin.display_text.clone(),
            coordinate: request.origin.coordinate,
            kind: "selected_place",
            via_id: None,
        }
    }

    pub(crate) fn destination_place(&self, request: &PlanningRequest) -> Place {
        Place {
            display_name: request.destination.display_text.clone(),
            coordinate: request.destination.coordinate,
            kind: "selected_place",
            via_id: None,
        }
    }

    pub(crate) fn stop_place(
        &self,
        stop_id: &crate::domain::ids::BoardingPointId,
    ) -> Option<Place> {
        self.transit_network.stop(stop_id).map(legs::transit_place)
    }

    pub(crate) fn station_place(
        &self,
        station_id: &crate::domain::ids::BikeShareStationId,
    ) -> Option<Place> {
        self.transit_network
            .bike_share_station(station_id)
            .map(legs::bike_place)
    }
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
