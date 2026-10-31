use time::{Duration, OffsetDateTime};

use crate::{
    domain::planning::PlanningInput,
    domain::{
        ids::BoardingPointId,
        journey::{Geometry, Leg, LegDetails, LegMode, Place, PlanningError},
    },
    routing::{
        journey::{JourneyAnchorKind, JourneyMovement, RealizedJourney},
        model::JourneyPlanningContext,
    },
    street::{
        Coordinate, ManeuverKind, Metres, ProviderFailureClass, RouteQuery, RoutingConstraints,
        Seconds, StreetPath, StreetTravelMode,
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
        append_routed_walking(&from, &to, path, &mut self.assembly);
        if matches!(
            movement.kind,
            crate::routing::JourneyMovementKind::Walking {
                kind: crate::routing::WalkingMovementKind::Transfer,
                ..
            }
        ) {
            register_pending_transfer(&self.assembly, &mut self.pending_transfer);
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

fn join_paths(paths: Vec<StreetPath>) -> StreetPath {
    let count = paths.len();
    let mut joined = StreetPath {
        duration: Seconds(0),
        distance: Metres(0),
        geometry: Vec::new(),
        maneuvers: Vec::new(),
        attribution: Vec::new(),
    };
    for (index, path) in paths.into_iter().enumerate() {
        let skip = usize::from(
            joined.geometry.last().is_some() && joined.geometry.last() == path.geometry.first(),
        );
        let offset = joined.geometry.len() - skip;
        joined.duration = Seconds(joined.duration.0.saturating_add(path.duration.0));
        joined.distance = Metres(joined.distance.0.saturating_add(path.distance.0));
        joined.geometry.extend(path.geometry.into_iter().skip(skip));
        joined.maneuvers.extend(
            path.maneuvers
                .into_iter()
                .filter(|maneuver| match maneuver.kind {
                    ManeuverKind::Depart => index == 0,
                    ManeuverKind::Arrive => index + 1 == count,
                    _ => true,
                })
                .map(|mut maneuver| {
                    maneuver.begin_geometry_index += offset;
                    maneuver.end_geometry_index =
                        maneuver.end_geometry_index.map(|end| end + offset);
                    maneuver
                }),
        );
        for attribution in path.attribution {
            if !joined.attribution.contains(&attribution) {
                joined.attribution.push(attribution);
            }
        }
    }
    joined
}

pub(super) fn append_routed_walking(
    from: &Place,
    to: &Place,
    path: StreetPath,
    assembly: &mut JourneyAssembly,
) {
    let departure = assembly.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    assembly.walking_seconds = assembly.walking_seconds.saturating_add(path.duration.0);
    assembly.arrival_time = arrival;
    evidence::push_unique_mode(&mut assembly.travel_modes, LegMode::Walking);
    let previous = assembly.walking_tail.take().filter(|(index, _)| {
        *index + 1 == assembly.legs.len()
            && assembly.legs[*index].arrival_time == departure
            && assembly.legs[*index].to.coordinate == from.coordinate
    });
    if let Some((index, previous_path)) = previous {
        let joined = join_paths(vec![previous_path, path]);
        let previous_leg = &assembly.legs[index];
        let leg = walking_leg(
            previous_leg.id.clone(),
            &previous_leg.from.clone(),
            to,
            previous_leg.departure_time,
            arrival,
            &joined,
        );
        assembly.legs[index] = leg;
        assembly.walking_tail = Some((index, joined));
        return;
    }
    let leg_id = format!("candidate_leg_{}", assembly.legs.len());
    assembly
        .legs
        .push(walking_leg(leg_id, from, to, departure, arrival, &path));
    assembly.walking_tail = Some((assembly.legs.len() - 1, path));
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
    async fn consecutive_walks_join_into_one_leg_preserving_geometry_and_timing_without_shortcuts()
    {
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
            assert_eq!(option.legs.len(), 1);
            let leg = &option.legs[0];
            assert_eq!(leg.mode, LegMode::Walking);
            assert_eq!(leg.from.display_name, "origin");
            assert_eq!(leg.to.display_name, "destination");
            assert_eq!(leg.geometry.points, geometry::encode_polyline6(&points));
            assert_eq!(leg.duration_seconds, 165);
            assert_eq!(leg.departure_time, departure);
            assert_eq!(leg.arrival_time, departure + Duration::seconds(165));
            assert_eq!(option.duration_seconds, 165);
            assert_eq!(option.walking_seconds, 165);
            assert_eq!(candidate.ranking.walking_seconds, 165);
            assert_eq!(
                candidate.ranking.arrival_epoch_seconds,
                (departure + Duration::seconds(165)).unix_timestamp()
            );
        }
    }

    #[test]
    fn joined_paths_drop_junction_maneuvers_and_shift_geometry_indexes() {
        let point = |longitude| Coordinate::try_new(25.0, longitude).unwrap();
        let attribution = crate::domain::journey::Attribution {
            text: "osm".into(),
            url: "https://www.openstreetmap.org/copyright".into(),
            licence: "ODbL".into(),
        };
        let path = |points: Vec<Coordinate>, duration| StreetPath {
            duration: Seconds(duration),
            distance: Metres(duration * 2),
            maneuvers: vec![
                crate::street::Maneuver::depart(0),
                crate::street::Maneuver::new(ManeuverKind::TurnLeft, 1),
                crate::street::Maneuver::arrive(points.len() - 1),
            ],
            geometry: points,
            attribution: vec![attribution.clone()],
        };

        let joined = join_paths(vec![
            path(vec![point(121.5), point(121.501), point(121.502)], 30),
            path(vec![point(121.502), point(121.503), point(121.504)], 45),
        ]);

        assert_eq!(joined.duration, Seconds(75));
        assert_eq!(joined.distance, Metres(150));
        assert_eq!(
            joined.geometry,
            [121.5, 121.501, 121.502, 121.503, 121.504].map(point)
        );
        assert_eq!(
            joined
                .maneuvers
                .iter()
                .map(|maneuver| (maneuver.kind, maneuver.begin_geometry_index))
                .collect::<Vec<_>>(),
            vec![
                (ManeuverKind::Depart, 0),
                (ManeuverKind::TurnLeft, 1),
                (ManeuverKind::TurnLeft, 3),
                (ManeuverKind::Arrive, 4),
            ]
        );
        assert_eq!(joined.attribution, vec![attribution]);
    }

    #[test]
    fn adjoining_walks_extend_the_previous_leg_only_when_place_and_time_match() {
        let departure = datetime!(2026-08-31 10:00 +08:00);
        let point = |longitude| Coordinate::try_new(25.0, longitude).unwrap();
        let place = |name: &str, longitude| Place {
            display_name: name.into(),
            coordinate: GeoCoordinate {
                latitude: 25.0,
                longitude,
            },
            via_id: None,
        };
        let path = |from, to, duration| StreetPath {
            duration: Seconds(duration),
            distance: Metres(duration),
            geometry: vec![point(from), point(to)],
            maneuvers: Vec::new(),
            attribution: Vec::new(),
        };
        let assembly = || JourneyAssembly {
            legs: Vec::new(),
            arrival_time: departure,
            live_evidence: Vec::new(),
            required_states: Vec::new(),
            travel_modes: Vec::new(),
            main_pattern_sequence: Vec::new(),
            transfer_evidence: Vec::new(),
            walking_seconds: 0,
            bicycle_seconds: 0,
            bike_share_uses: 0,
            bike_reuse_available_at: None,
            timing_bases: Vec::new(),
            timing_reliability: crate::domain::transit_timing::TimingReliability::Reliable,
            walking_tail: None,
        };
        let (a, b, c) = (place("a", 121.5), place("b", 121.501), place("c", 121.502));

        let mut joined = assembly();
        append_routed_walking(&a, &b, path(121.5, 121.501, 30), &mut joined);
        append_routed_walking(&b, &c, path(121.501, 121.502, 45), &mut joined);
        assert_eq!(joined.legs.len(), 1);
        assert_eq!(joined.legs[0].from.display_name, "a");
        assert_eq!(joined.legs[0].to.display_name, "c");
        assert_eq!(joined.legs[0].duration_seconds, 75);
        assert_eq!(
            joined.legs[0].arrival_time,
            departure + Duration::seconds(75)
        );
        assert_eq!(joined.walking_seconds, 75);

        let mut waited = assembly();
        append_routed_walking(&a, &b, path(121.5, 121.501, 30), &mut waited);
        waited.arrival_time += Duration::minutes(5);
        append_routed_walking(&b, &c, path(121.501, 121.502, 45), &mut waited);
        assert_eq!(waited.legs.len(), 2);

        let mut elsewhere = assembly();
        append_routed_walking(&a, &b, path(121.5, 121.501, 30), &mut elsewhere);
        append_routed_walking(&a, &c, path(121.5, 121.502, 45), &mut elsewhere);
        assert_eq!(elsewhere.legs.len(), 2);
    }
}
