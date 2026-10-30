use time::{Duration, OffsetDateTime};

use crate::{
    domain::planning::PlanningInput,
    domain::{
        ids::BoardingPointId,
        journey::{Geometry, Leg, LegDetails, LegMode, Place, PlanningError},
    },
    routing::{
        model::JourneyPlanningContext,
        realized::{JourneyAnchorKind, JourneyMovement, RealizedJourney},
    },
    street::{
        Coordinate, ProviderFailureClass, RouteQuery, RoutingConstraints, StreetPath,
        StreetTravelMode,
    },
};

use super::{
    JourneyReconstructor,
    assembler::{JourneyAssembly, ReconstructionState},
    evidence, geometry,
    transit::register_pending_transfer,
};

impl ReconstructionState<'_, '_> {
    pub(super) async fn append_walk(
        &mut self,
        movement: &JourneyMovement,
    ) -> Result<bool, PlanningError> {
        let from = anchor_place(
            self.reconstructor,
            self.context.request,
            self.journey,
            movement.from_anchor,
        )?;
        let to = anchor_place(
            self.reconstructor,
            self.context.request,
            self.journey,
            movement.to_anchor,
        )?;
        let Some(path) = hydrate_path(
            self.reconstructor,
            self.context,
            &from,
            &to,
            StreetTravelMode::Walking,
        )
        .await?
        else {
            return Ok(false);
        };
        let connector_start = self.assembly.legs.len();
        append_routed_walking(&from, &to, path, &mut self.assembly);
        if matches!(
            movement.kind,
            crate::routing::JourneyMovementKind::Walking {
                kind: crate::routing::WalkingMovementKind::Transfer,
                ..
            }
        ) {
            register_pending_transfer(&self.assembly, &mut self.pending_transfer, connector_start);
        }
        self.current_stop = anchor_boarding_point(self.journey, movement.to_anchor);
        Ok(true)
    }
}

pub(super) fn anchor_boarding_point(
    journey: &RealizedJourney,
    anchor: usize,
) -> Option<BoardingPointId> {
    match &journey.anchors()[anchor].kind {
        JourneyAnchorKind::BoardingPoint { id } => Some(id.clone()),
        JourneyAnchorKind::Origin | JourneyAnchorKind::Destination => None,
    }
}

pub(super) fn anchor_place(
    reconstructor: &JourneyReconstructor,
    request: &PlanningInput,
    journey: &RealizedJourney,
    anchor: usize,
) -> Result<Place, PlanningError> {
    let anchor = journey
        .anchors()
        .get(anchor)
        .ok_or(PlanningError::Internal)?;
    match &anchor.kind {
        JourneyAnchorKind::Origin => Ok(reconstructor.origin_place(request)),
        JourneyAnchorKind::BoardingPoint { id } => reconstructor.stop_place(id),
        JourneyAnchorKind::Destination => Ok(reconstructor.destination_place(request)),
    }
}

pub(super) fn append_routed_walking(
    from: &Place,
    to: &Place,
    path: StreetPath,
    assembly: &mut JourneyAssembly,
) {
    let departure = assembly.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", assembly.legs.len());
    assembly
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    assembly.walking_seconds = assembly.walking_seconds.saturating_add(path.duration.0);
    assembly.arrival_time = arrival;
    evidence::push_unique_mode(&mut assembly.travel_modes, LegMode::Walking);
}

pub(super) async fn hydrate_path(
    reconstructor: &JourneyReconstructor,
    context: &JourneyPlanningContext<'_>,
    from: &Place,
    to: &Place,
    mode: StreetTravelMode,
) -> Result<Option<StreetPath>, PlanningError> {
    context.ensure_before_deadline()?;
    let from = Coordinate::try_from(from.coordinate).map_err(|_| PlanningError::Internal)?;
    let to = Coordinate::try_from(to.coordinate).map_err(|_| PlanningError::Internal)?;
    let result = reconstructor
        .street_routing
        .route(RouteQuery {
            travel_mode: mode,
            origin: from,
            destination: to,
            constraints: RoutingConstraints::default(),
        })
        .await;
    context.ensure_before_deadline()?;
    match result {
        Ok(path) => Ok(Some(path)),
        Err(error) => match error.failure_class() {
            ProviderFailureClass::Unreachable => Ok(None),
            ProviderFailureClass::Transient => Err(PlanningError::TemporarilyUnavailable),
            ProviderFailureClass::Internal => Err(PlanningError::Internal),
        },
    }
}

fn walking_leg(
    id: String,
    from: &Place,
    to: &Place,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
    path: &StreetPath,
) -> Leg {
    Leg {
        id,
        mode: LegMode::Walking,
        from: from.clone(),
        to: to.clone(),
        departure_time,
        arrival_time,
        duration_seconds: path.duration.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: geometry::encode_polyline6(&path.geometry),
        },
        guidance: geometry::guidance(&path.maneuvers),
        details: LegDetails::Walking,
        attribution: path.attribution.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{
            bikeshare::BikeShareNetwork,
            location::GeoCoordinate,
            transit::{TransitNetwork, TransitStop},
            transit_runtime::TransitRuntime,
        },
        routing::{JourneyAnchor, JourneyMovementKind, PlanningDeadline, WalkingMovementKind},
        street::{FakeStreetRouter, Metres, Seconds},
    };
    use std::sync::Arc;
    use time::macros::datetime;

    #[tokio::test]
    async fn transfer_then_egress_preserves_geometry_and_actual_timing_without_shortcuts() {
        let departure = datetime!(2026-08-31 10:00 +08:00);
        let points = [121.5, 121.501, 121.502, 121.503]
            .map(|longitude| Coordinate::try_new(25.0, longitude).unwrap());
        let geo = |index: usize| GeoCoordinate {
            latitude: points[index].latitude(),
            longitude: points[index].longitude(),
        };
        let stops = [("a", 1), ("b", 2)]
            .map(|(id, index)| TransitStop::new(BoardingPointId::new(id), id, geo(index)));
        let network = Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::fixture(
            time::Time::MIDNIGHT,
            stops.to_vec(),
            Vec::new(),
            Vec::new(),
        ))));
        let request = serde_json::from_value::<crate::api::PlanningRequest>(serde_json::json!({
            "origin": {"coordinate": geo(0), "display_text": "origin"},
            "destination": {"coordinate": geo(3), "display_text": "destination"},
            "selected_modes": [], "departure_setting": {"type": "depart_now"}
        }))
        .unwrap();
        let request: crate::domain::planning::PlanningInput = request.into();
        let context =
            JourneyPlanningContext::new(&request, departure, 14400, PlanningDeadline::disabled())
                .unwrap();
        let kinds = [
            JourneyAnchorKind::Origin,
            JourneyAnchorKind::BoardingPoint {
                id: BoardingPointId::new("a"),
            },
            JourneyAnchorKind::BoardingPoint {
                id: BoardingPointId::new("b"),
            },
            JourneyAnchorKind::Destination,
        ];
        let anchors = kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| JourneyAnchor {
                kind,
                reached_at: departure + Duration::seconds(index as i64 * 60),
            })
            .collect();
        let movements = [
            WalkingMovementKind::Access,
            WalkingMovementKind::Transfer,
            WalkingMovementKind::Egress,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| JourneyMovement {
            from_anchor: index,
            to_anchor: index + 1,
            departure_at: departure + Duration::seconds(index as i64 * 60),
            arrival_at: departure + Duration::seconds((index + 1) as i64 * 60),
            kind: JourneyMovementKind::Walking {
                kind,
                duration_seconds: 60,
                distance_metres: 100,
            },
        })
        .collect();
        let journey = RealizedJourney::from_movements(anchors, movements, departure).unwrap();
        let path = |from: usize, to: usize, duration| StreetPath {
            duration: Seconds(duration),
            distance: Metres(100),
            geometry: vec![points[from], points[to]],
            maneuvers: Vec::new(),
            attribution: Vec::new(),
        };
        for reachable in [true, false] {
            let mut street = FakeStreetRouter::builder()
                .path(
                    StreetTravelMode::Walking,
                    points[0],
                    points[1],
                    path(0, 1, 30),
                )
                .path(
                    StreetTravelMode::Walking,
                    points[2],
                    points[3],
                    path(2, 3, 90),
                )
                .path(
                    StreetTravelMode::Walking,
                    points[1],
                    points[3],
                    path(1, 3, 1),
                );
            if reachable {
                street = street.path(
                    StreetTravelMode::Walking,
                    points[1],
                    points[2],
                    path(1, 2, 45),
                );
            }
            let reconstructor = JourneyReconstructor::new(
                Arc::new(street.build()),
                network.clone(),
                Arc::new(BikeShareNetwork::new(Vec::new())),
            );
            let candidate = reconstructor.reconstruct(&journey, &context).await.unwrap();
            if !reachable {
                assert!(candidate.is_none());
                continue;
            }
            let candidate = candidate.unwrap();
            let option = &candidate.option;
            assert_eq!(option.legs.len(), 3);
            assert_eq!(option.legs[1].from.via_id.as_deref(), Some("a"));
            assert_eq!(option.legs[1].to.via_id.as_deref(), Some("b"));
            assert_eq!(
                option.legs[1].geometry.points,
                geometry::encode_polyline6(&[points[1], points[2]])
            );
            assert_eq!(
                option.legs[2].geometry.points,
                geometry::encode_polyline6(&[points[2], points[3]])
            );
            assert_eq!(
                option
                    .legs
                    .iter()
                    .map(|leg| leg.duration_seconds)
                    .collect::<Vec<_>>(),
                vec![30, 45, 90]
            );
            assert_eq!(option.legs[1].arrival_time, option.legs[2].departure_time);
            assert_eq!(option.duration_seconds, 165);
            assert_eq!(option.walking_seconds, 165);
            assert_eq!(candidate.ranking.walking_seconds, 165);
            assert_eq!(
                candidate.ranking.arrival_epoch_seconds,
                (departure + Duration::seconds(165)).unix_timestamp()
            );
        }
    }
}
