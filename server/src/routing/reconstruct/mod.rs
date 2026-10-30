mod session;
#[cfg(test)]
pub(in crate::routing) use session::ordered_reconstruction;
mod assembler;
mod bike;
mod evidence;
mod geometry;
mod street;
mod transit;

use std::sync::Arc;

use crate::{
    domain::planning::PlanningInput,
    domain::{
        bikeshare::BikeShareNetwork,
        ids::BikeStationId,
        journey::{JourneyOption, Place, PlanningError},
        ranking::{RankedJourney, RankingCandidate},
        transit_runtime::TransitRuntime,
    },
    street::StreetRouter,
};

use super::{model::JourneyPlanningContext, realized::RealizedJourney};

const BASELINE_MATCHING_KEY_VERSION: &str = "candidate:v2";
const BIKE_MATCHING_KEY_VERSION: &str = "candidate:bike:v1";

pub(super) use geometry::{taipei_now, to_street_coordinate};

#[derive(Clone)]
pub(super) struct JourneyReconstructor {
    street_routing: Arc<dyn StreetRouter>,
    transit_runtime: Arc<TransitRuntime>,
    bike_share_network: Arc<BikeShareNetwork>,
}

pub(super) struct JourneyCandidate {
    pub(super) ranking: RankingCandidate,
    pub(super) option: JourneyOption,
}

impl JourneyReconstructor {
    pub(super) fn new(
        street_routing: Arc<dyn StreetRouter>,
        transit_runtime: Arc<TransitRuntime>,
        bike_share_network: Arc<BikeShareNetwork>,
    ) -> Self {
        Self {
            street_routing,
            transit_runtime,
            bike_share_network,
        }
    }

    pub(super) fn session(&self) -> session::ReconstructionSession {
        session::ReconstructionSession::new(self.request_local())
    }

    pub(super) fn request_local(&self) -> Self {
        Self {
            street_routing: Arc::new(super::cache::RouteCache::new(self.street_routing.clone())),
            transit_runtime: self.transit_runtime.clone(),
            bike_share_network: self.bike_share_network.clone(),
        }
    }

    pub(super) async fn reconstruct(
        &self,
        journey: &RealizedJourney,
        context: &JourneyPlanningContext<'_>,
    ) -> Result<Option<JourneyCandidate>, PlanningError> {
        let Some(assembled) = assembler::assemble(self, journey, context).await? else {
            return Ok(None);
        };
        if context
            .latest_arrival
            .is_some_and(|latest| assembled.arrival_time > latest)
        {
            return Ok(None);
        }
        let duration_seconds =
            transit::seconds_between(journey.departure_at(), assembled.arrival_time)
                .ok_or(PlanningError::Internal)?;
        if duration_seconds > context.max_journey_duration_seconds {
            return Ok(None);
        }
        let transfer_count = assembled.transfer_evidence.len() as u32;
        let matching_key = journey_matching_key(journey);
        let option = JourneyOption {
            id: "candidate".into(),
            matching_key: matching_key.clone(),
            label: None,
            departure_time: journey.departure_at(),
            arrival_time: assembled.arrival_time,
            duration_seconds,
            walking_seconds: assembled.walking_seconds,
            transfer_count,
            travel_modes: assembled.travel_modes,
            live_status_summary: evidence::summary_from_evidence(&assembled.live_evidence),
            overview_geometry: geometry::overview_geometry(&assembled.legs),
            legs: assembled.legs,
        };
        let stable_key = format!(
            "{}:{}",
            matching_key,
            journey.departure_at().unix_timestamp()
        );
        Ok(Some(JourneyCandidate {
            ranking: RankingCandidate {
                stable_key,
                departure_epoch_seconds: journey.departure_at().unix_timestamp(),
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

    pub(super) fn origin_place(&self, request: &PlanningInput) -> Place {
        Place {
            display_name: request.origin.display_text.clone(),
            coordinate: request.origin.coordinate,
            via_id: None,
        }
    }

    pub(super) fn destination_place(&self, request: &PlanningInput) -> Place {
        Place {
            display_name: request.destination.display_text.clone(),
            coordinate: request.destination.coordinate,
            via_id: None,
        }
    }

    pub(super) fn stop_place(
        &self,
        stop_id: &crate::domain::ids::BoardingPointId,
    ) -> Result<Place, PlanningError> {
        self.transit_runtime
            .stop(stop_id)
            .map(transit::transit_place)
            .ok_or(PlanningError::Internal)
    }

    pub(super) fn bike_station_place(
        &self,
        station_id: &BikeStationId,
    ) -> Result<Place, PlanningError> {
        self.bike_share_network
            .station(station_id)
            .map(|station| Place {
                display_name: station.name.clone(),
                coordinate: station.coordinate,
                via_id: Some(station.id.as_str().into()),
            })
            .ok_or(PlanningError::Internal)
    }
}

fn journey_matching_key(journey: &RealizedJourney) -> String {
    let version = if !journey.has_bike_share() {
        BASELINE_MATCHING_KEY_VERSION
    } else {
        BIKE_MATCHING_KEY_VERSION
    };
    geometry::short_digest(&format!(
        "{version}:{}",
        journey.identity().canonical_signature()
    ))
}

pub(super) fn apply_ranked_journey(option: &mut JourneyOption, ranked: &RankedJourney) {
    option.label = ranked.label;
}

pub(super) fn assign_result_local_ids(option: &mut JourneyOption, index: usize) {
    let suffix = geometry::short_digest(&format!("{}:{index}", option.matching_key));
    option.id = format!("option_{:02}_{}", index + 1, suffix);
    for (leg_index, leg) in option.legs.iter_mut().enumerate() {
        leg.id = format!("leg_{:02}_{}_{suffix}", index + 1, leg_index + 1);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{BASELINE_MATCHING_KEY_VERSION, BIKE_MATCHING_KEY_VERSION, JourneyReconstructor};
    use crate::{
        domain::{
            bikeshare::BikeShareNetwork,
            ids::{BikeStationId, BoardingPointId},
            journey::PlanningError,
            transit::TransitNetwork,
            transit_runtime::TransitRuntime,
        },
        routing::reconstruct::geometry::short_digest,
        street::FakeStreetRouter,
    };

    fn empty_reconstructor() -> JourneyReconstructor {
        JourneyReconstructor::new(
            Arc::new(FakeStreetRouter::builder().build()),
            Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::default()))),
            Arc::new(BikeShareNetwork::new(Vec::new())),
        )
    }

    #[tokio::test]
    async fn cached_walking_paths_keep_candidate_timelines_independent() {
        use crate::{
            domain::location::GeoCoordinate,
            routing::{
                JourneyAnchor, JourneyAnchorKind, JourneyMovement, JourneyMovementKind,
                PlanningDeadline, RealizedJourney, WalkingMovementKind,
                model::JourneyPlanningContext,
            },
            street::{Coordinate, Metres, Seconds, StreetPath, StreetTravelMode},
        };
        use time::{Duration, macros::datetime};
        let start = datetime!(2026-08-31 10:00 +08:00);
        let origin = Coordinate::try_new(25.0, 121.5).unwrap();
        let destination = Coordinate::try_new(25.01, 121.51).unwrap();
        let geo = |p: Coordinate| GeoCoordinate {
            latitude: p.latitude(),
            longitude: p.longitude(),
        };
        let request = serde_json::from_value::<crate::api::PlanningRequest>(serde_json::json!({"origin":{"coordinate":geo(origin),"display_text":"origin"},"destination":{"coordinate":geo(destination),"display_text":"destination"},"selected_modes":[],"departure_setting":{"type":"depart_now"}})).unwrap();
        let request: crate::domain::planning::PlanningInput = request.into();
        let context =
            JourneyPlanningContext::new(&request, start, 14400, PlanningDeadline::disabled())
                .unwrap();
        let street = FakeStreetRouter::builder()
            .path(
                StreetTravelMode::Walking,
                origin,
                destination,
                StreetPath {
                    duration: Seconds(180),
                    distance: Metres(100),
                    geometry: vec![origin, destination],
                    maneuvers: vec![],
                    attribution: vec![],
                },
            )
            .build();
        let reconstructor = JourneyReconstructor::new(
            Arc::new(street),
            Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::default()))),
            Arc::new(BikeShareNetwork::new(Vec::new())),
        )
        .request_local();
        let journey = |departure| {
            RealizedJourney::from_movements(
                vec![
                    JourneyAnchor {
                        kind: JourneyAnchorKind::Origin,
                        reached_at: departure,
                    },
                    JourneyAnchor {
                        kind: JourneyAnchorKind::Destination,
                        reached_at: departure + Duration::seconds(180),
                    },
                ],
                vec![JourneyMovement {
                    from_anchor: 0,
                    to_anchor: 1,
                    departure_at: departure,
                    arrival_at: departure + Duration::seconds(180),
                    kind: JourneyMovementKind::Walking {
                        kind: WalkingMovementKind::Direct,
                        duration_seconds: 180,
                        distance_metres: 100,
                    },
                }],
                departure,
            )
            .unwrap()
        };
        let first = reconstructor
            .reconstruct(&journey(start), &context)
            .await
            .unwrap()
            .unwrap()
            .option;
        let second = reconstructor
            .reconstruct(&journey(start + Duration::minutes(15)), &context)
            .await
            .unwrap()
            .unwrap()
            .option;
        assert_eq!(
            first.legs[0].geometry.points,
            second.legs[0].geometry.points
        );
        assert_eq!(first.legs[0].arrival_time, start + Duration::seconds(180));
        assert_eq!(
            second.legs[0].arrival_time,
            start + Duration::minutes(15) + Duration::seconds(180)
        );
    }

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

    #[test]
    fn missing_required_catalog_places_are_internal_errors() {
        let reconstructor = empty_reconstructor();

        assert!(matches!(
            reconstructor.stop_place(&BoardingPointId::new("missing-stop")),
            Err(PlanningError::Internal)
        ));
        assert!(matches!(
            reconstructor.bike_station_place(&BikeStationId::new("missing-station")),
            Err(PlanningError::Internal)
        ));
    }
}
