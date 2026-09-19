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
    augmentation::JourneyVariant,
    service::{GeneratedCandidate, PlanningContext, RoutingService},
};

pub(crate) use geometry::{taipei_now, to_street_coordinate};

impl RoutingService {
    pub(crate) async fn reconstruct_multimodal_variant(
        &self,
        request: &PlanningRequest,
        variant: &JourneyVariant,
        context: &PlanningContext,
    ) -> Result<Option<GeneratedCandidate>, PlanningError> {
        let candidate = variant.skeleton().candidate();
        let Some(assembled) = legs::assemble(self, request, variant, context).await? else {
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
        let transfer_count = assembled.transfers.len() as u32;
        let matching_key = if variant.is_baseline() {
            geometry::short_digest(&format!("candidate:v2:{}", variant.signature()))
        } else {
            geometry::short_digest(&format!("candidate:bike:v1:{}", variant.signature()))
        };
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
            bicycle_seconds: assembled.bicycle_seconds,
            bike_share_uses: assembled.bike_share_uses,
            transfer_count,
            travel_modes: assembled.travel_modes,
            live_status_summary: evidence::summary_from_evidence(&assembled.live_evidence),
            overview_geometry: geometry::overview_geometry(&assembled.legs),
            legs: assembled.legs,
            transfers: assembled.transfers,
            warnings: Vec::new(),
        };
        let stable_key = format!(
            "{}:{}",
            matching_key,
            candidate.departure_time.unix_timestamp()
        );
        Ok(Some(GeneratedCandidate {
            ranking: crate::domain::ranking::CandidateJourney {
                stable_key,
                departure_epoch_seconds: candidate.departure_time.unix_timestamp(),
                arrival_epoch_seconds: assembled.arrival_time.unix_timestamp(),
                walking_seconds: assembled.walking_seconds,
                bicycle_seconds: assembled.bicycle_seconds,
                bike_share_uses: assembled.bike_share_uses,
                transfer_count,
                main_service_sequence: assembled.main_service_sequence,
                timing_bases: assembled.timing_bases,
                timing_reliability: assembled.timing_reliability,
                required_live_status: assembled.required_states,
                transfers: assembled.transfer_evidence,
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
            bicycle_seconds: 0,
            bike_share_uses: 0,
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
                bicycle_seconds: 0,
                bike_share_uses: 0,
                transfer_count: 0,
                main_service_sequence: Vec::new(),
                timing_bases: Vec::new(),
                timing_reliability: crate::domain::transit_timing::TimingReliability::Reliable,
                required_live_status: Vec::new(),
                transfers: Vec::new(),
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

    pub(crate) fn bike_station_place(
        &self,
        station_id: &crate::domain::ids::BikeStationId,
    ) -> Option<Place> {
        self.bike_share_network
            .as_ref()?
            .station(station_id)
            .map(|station| Place {
                display_name: station.name.clone(),
                coordinate: station.coordinate,
                kind: "bike_station",
                via_id: Some(station.id.as_str().into()),
            })
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
