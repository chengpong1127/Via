mod assembler;
mod bike;
mod evidence;
mod geometry;
mod street;
mod transit;

use std::collections::HashMap;

use crate::{
    api::PlanningRequest,
    domain::{
        journey::{JourneyClass, JourneyOption, Place, PlanningError},
        ranking::RankedJourney,
    },
};

use super::{
    service::{PlanningContext, ReconstructedCandidate, RoutingService},
    variant::JourneyVariant,
};

const BASELINE_MATCHING_KEY_VERSION: &str = "candidate:v2";
const BIKE_MATCHING_KEY_VERSION: &str = "candidate:bike:v1";

pub(super) use geometry::{taipei_now, to_street_coordinate};

impl RoutingService {
    pub(super) async fn reconstruct_multimodal_variant(
        &self,
        request: &PlanningRequest,
        variant: &JourneyVariant,
        context: &PlanningContext,
    ) -> Result<Option<ReconstructedCandidate>, PlanningError> {
        let Some(assembled) = assembler::assemble(self, request, variant, context).await? else {
            return Ok(None);
        };
        if context
            .latest_arrival
            .is_some_and(|latest| assembled.arrival_time > latest)
        {
            return Ok(None);
        }
        let duration_seconds =
            transit::seconds_between(variant.skeleton().departure_at(), assembled.arrival_time)
                .ok_or(PlanningError::Internal)?;
        let transfer_count = assembled.transfers.len() as u32;
        let matching_key = variant_matching_key(variant);
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key: matching_key.clone(),
            class: JourneyClass::Reliable,
            label: None,
            ranking_reasons: Vec::new(),
            departure_time: variant.skeleton().departure_at(),
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
            variant.skeleton().departure_at().unix_timestamp()
        );
        Ok(Some(ReconstructedCandidate {
            ranking: crate::domain::ranking::RankingCandidate {
                stable_key,
                departure_epoch_seconds: variant.skeleton().departure_at().unix_timestamp(),
                arrival_epoch_seconds: assembled.arrival_time.unix_timestamp(),
                walking_seconds: assembled.walking_seconds,
                bicycle_seconds: assembled.bicycle_seconds,
                bike_share_uses: assembled.bike_share_uses,
                transfer_count,
                main_pattern_sequence: assembled.main_pattern_sequence,
                timing_bases: assembled.timing_bases,
                timing_reliability: assembled.timing_reliability,
                required_live_status: assembled.required_states,
                transfers: assembled.transfer_evidence,
                blocking_condition: false,
            },
            option,
        }))
    }

    pub(super) fn origin_place(&self, request: &PlanningRequest) -> Place {
        Place {
            display_name: request.origin.display_text.clone(),
            coordinate: request.origin.coordinate,
            kind: "selected_place",
            via_id: None,
        }
    }

    pub(super) fn destination_place(&self, request: &PlanningRequest) -> Place {
        Place {
            display_name: request.destination.display_text.clone(),
            coordinate: request.destination.coordinate,
            kind: "selected_place",
            via_id: None,
        }
    }

    pub(super) fn stop_place(
        &self,
        stop_id: &crate::domain::ids::BoardingPointId,
    ) -> Option<Place> {
        self.transit_runtime
            .stop(stop_id)
            .map(transit::transit_place)
    }

    pub(super) fn bike_station_place(
        &self,
        station_id: &crate::domain::ids::BikeStationId,
    ) -> Option<Place> {
        self.bike_share_network
            .station(station_id)
            .map(|station| Place {
                display_name: station.name.clone(),
                coordinate: station.coordinate,
                kind: "bike_station",
                via_id: Some(station.id.as_str().into()),
            })
    }
}

fn variant_matching_key(variant: &JourneyVariant) -> String {
    let version = if variant.is_baseline() {
        BASELINE_MATCHING_KEY_VERSION
    } else {
        BIKE_MATCHING_KEY_VERSION
    };
    geometry::short_digest(&format!(
        "{version}:{}",
        variant.identity().canonical_signature()
    ))
}

pub(super) fn apply_ranked_journey(option: &mut JourneyOption, ranked: &RankedJourney) {
    option.class = ranked.class;
    option.label = ranked.label;
    option.ranking_reasons = ranked.ranking_reasons.clone();
}

pub(super) fn assign_result_local_ids(option: &mut JourneyOption, index: usize) {
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

#[cfg(test)]
mod tests {
    use super::{BASELINE_MATCHING_KEY_VERSION, BIKE_MATCHING_KEY_VERSION};
    use crate::routing::reconstruct::geometry::short_digest;

    #[test]
    fn matching_key_versions_remain_stable() {
        let baseline_signature = "access:a:0|transfer:a:b:60|transfer:b:c:60|egress:c:0";
        let bike_signature = "path:v2:baseline:walking:Access:origin:boarding_point:a:0:0|bike:boarding_point:a:boarding_point:c:pickup:dropoff:0:0:30:100:0:0:0:30|baseline:walking:Egress:boarding_point:c:destination:0:0";

        assert_eq!(
            short_digest(&format!(
                "{BASELINE_MATCHING_KEY_VERSION}:{baseline_signature}"
            )),
            "match_60a8b9c214a735ea"
        );
        assert_eq!(
            short_digest(&format!("{BIKE_MATCHING_KEY_VERSION}:{bike_signature}")),
            "match_15eae0d1afa8b091"
        );
    }
}
