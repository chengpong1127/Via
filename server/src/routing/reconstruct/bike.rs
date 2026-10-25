use time::Duration;

use crate::{
    domain::journey::{Geometry, Guidance, Leg, LegDetails, LegMode, Place, PlanningError},
    routing::{JourneyAnchorKind, JourneyMovement, model::JourneyPlanningContext},
    street::{StreetPath, StreetTravelMode},
};

use super::{
    JourneyReconstructor,
    assembler::{JourneyAssembly, ReconstructionState},
    evidence, geometry,
    street::{anchor_boarding_point, anchor_place, append_routed_walking, hydrate_path},
    transit::register_pending_transfer,
};

impl ReconstructionState<'_, '_> {
    pub(super) async fn append_bike(
        &mut self,
        movement: &JourneyMovement,
        bike_movement: &crate::routing::BikeShareMovement,
    ) -> Result<bool, PlanningError> {
        if bike_movement.from_anchor != movement.from_anchor
            || bike_movement.to_anchor != movement.to_anchor
        {
            return Ok(false);
        }
        if matches!(
            self.journey.anchors()[movement.from_anchor].kind,
            JourneyAnchorKind::Destination
        ) || matches!(
            self.journey.anchors()[movement.to_anchor].kind,
            JourneyAnchorKind::Origin
        ) {
            return Ok(false);
        }
        let source_stop = anchor_boarding_point(self.journey, movement.from_anchor);
        if source_stop
            .as_ref()
            .is_some_and(|source_stop| self.current_stop.as_ref() != Some(source_stop))
        {
            return Ok(false);
        }
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
        let connector_start = self.assembly.legs.len();
        if !append_bike_share(
            self.reconstructor,
            self.context,
            &from,
            &to,
            bike_movement,
            &mut self.assembly,
        )
        .await?
        {
            return Ok(false);
        }
        register_pending_transfer(&self.assembly, &mut self.pending_transfer, connector_start);
        if let Some(to_stop) = anchor_boarding_point(self.journey, movement.to_anchor) {
            self.current_stop = Some(to_stop);
        }

        Ok(true)
    }
}

async fn append_bike_share(
    reconstructor: &JourneyReconstructor,
    context: &JourneyPlanningContext<'_>,
    from: &Place,
    to: &Place,
    bike_movement: &crate::routing::BikeShareMovement,
    assembly: &mut JourneyAssembly,
) -> Result<bool, PlanningError> {
    let pickup = reconstructor.bike_station_place(&bike_movement.pickup_station_id)?;
    let dropoff = reconstructor.bike_station_place(&bike_movement.dropoff_station_id)?;

    if let Some(available_at) = assembly.bike_reuse_available_at {
        assembly.arrival_time = assembly.arrival_time.max(available_at);
    }

    let Some(to_pickup) = hydrate_path(
        reconstructor,
        context,
        from,
        &pickup,
        StreetTravelMode::Walking,
        assembly.arrival_time,
    )
    .await?
    else {
        return Ok(false);
    };
    append_routed_walking(from, &pickup, to_pickup, assembly);

    let Some(bicycle) = hydrate_path(
        reconstructor,
        context,
        &pickup,
        &dropoff,
        StreetTravelMode::Bicycle,
        assembly.arrival_time,
    )
    .await?
    else {
        return Ok(false);
    };
    append_bicycle(
        &pickup,
        &dropoff,
        &bike_movement.pickup_station_id,
        &bike_movement.dropoff_station_id,
        bicycle,
        assembly,
    );

    assembly.bike_reuse_available_at = Some(assembly.arrival_time + Duration::minutes(15));

    let Some(from_dropoff) = hydrate_path(
        reconstructor,
        context,
        &dropoff,
        to,
        StreetTravelMode::Walking,
        assembly.arrival_time,
    )
    .await?
    else {
        return Ok(false);
    };
    append_routed_walking(&dropoff, to, from_dropoff, assembly);
    assembly.bike_share_uses = assembly.bike_share_uses.saturating_add(1);
    Ok(true)
}

fn append_bicycle(
    pickup: &Place,
    dropoff: &Place,
    pickup_station_id: &crate::domain::ids::BikeStationId,
    dropoff_station_id: &crate::domain::ids::BikeStationId,
    path: StreetPath,
    assembly: &mut JourneyAssembly,
) {
    let departure = assembly.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", assembly.legs.len());
    assembly.legs.push(Leg {
        id: leg_id,
        mode: LegMode::Bicycle,
        from: pickup.clone(),
        to: dropoff.clone(),
        departure_time: departure,
        arrival_time: arrival,
        duration_seconds: path.duration.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: geometry::encode_polyline6(&path.geometry),
        },
        guidance: vec![Guidance {
            code: "bike_share",
            parameters: std::collections::BTreeMap::from([
                ("pickup_station_name".into(), pickup.display_name.clone()),
                ("dropoff_station_name".into(), dropoff.display_name.clone()),
            ]),
        }],
        details: LegDetails::BikeShare {
            pickup_station_id: pickup_station_id.as_str().into(),
            pickup_station_name: pickup.display_name.clone(),
            dropoff_station_id: dropoff_station_id.as_str().into(),
            dropoff_station_name: dropoff.display_name.clone(),
        },
        attribution: path.attribution.clone(),
    });
    assembly.bicycle_seconds = assembly.bicycle_seconds.saturating_add(path.duration.0);
    assembly.arrival_time = arrival;
    evidence::push_unique_mode(&mut assembly.travel_modes, LegMode::Bicycle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{
            bikeshare::{BikeShareNetwork, BikeStation, BikeWalkingAccess},
            ids::BikeStationId,
            location::GeoCoordinate,
            transit::TransitNetwork,
            transit_runtime::TransitRuntime,
            transit_timing::TimingReliability,
        },
        routing::{BikeRide, BikeShareMovement, PlanningDeadline},
        street::{Coordinate, FakeStreetRouter, Metres, Seconds},
    };
    use std::sync::Arc;
    use time::macros::datetime;

    #[tokio::test]
    async fn rental_reuse_uses_actual_return_and_exit_walk_instead_of_stale_p2_wait() {
        let departure = datetime!(2026-08-31 10:00 +08:00);
        let coordinate = |longitude| Coordinate::try_new(25.0, longitude).unwrap();
        let points = [121.5, 121.501, 121.502, 121.503, 121.504, 121.505, 121.506].map(coordinate);
        let place = |index: usize| Place {
            display_name: index.to_string(),
            coordinate: GeoCoordinate {
                latitude: points[index].latitude(),
                longitude: points[index].longitude(),
            },
            via_id: None,
        };
        let stations = [1, 2, 4, 5].map(|index| {
            BikeStation::new(
                BikeStationId::new(index.to_string()),
                index.to_string(),
                place(index).coordinate,
            )
        });
        let mut street = FakeStreetRouter::builder();
        for (from, to, mode, duration) in [
            (0, 1, StreetTravelMode::Walking, 20),
            (1, 2, StreetTravelMode::Bicycle, 40),
            (2, 3, StreetTravelMode::Walking, 300),
            (3, 4, StreetTravelMode::Walking, 10),
            (4, 5, StreetTravelMode::Bicycle, 30),
            (5, 6, StreetTravelMode::Walking, 20),
        ] {
            street = street.path(
                mode,
                points[from],
                points[to],
                StreetPath {
                    duration: Seconds(duration),
                    distance: Metres(100),
                    geometry: vec![points[from], points[to]],
                    maneuvers: Vec::new(),
                    attribution: Vec::new(),
                },
            );
        }
        let reconstructor = JourneyReconstructor::new(
            Arc::new(street.build()),
            Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::default()))),
            Arc::new(BikeShareNetwork::new(stations.to_vec())),
        );
        let request = serde_json::from_value(serde_json::json!({
            "origin": {"coordinate": place(0).coordinate, "display_text": "origin"},
            "destination": {"coordinate": place(6).coordinate, "display_text": "destination"},
            "selected_modes": ["bike_share"],
            "departure_setting": {"type": "depart_now"}
        }))
        .unwrap();
        let context =
            JourneyPlanningContext::new(&request, departure, 14400, PlanningDeadline::disabled())
                .unwrap();
        let mut assembly = JourneyAssembly {
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
            timing_reliability: TimingReliability::Reliable,
        };
        let movement = |from, to, pickup: &str, dropoff: &str| {
            BikeShareMovement::new(
                from,
                to,
                BikeStationId::new(pickup),
                BikeStationId::new(dropoff),
                BikeWalkingAccess::new(0, 0),
                BikeRide {
                    duration_seconds: 60,
                    distance_metres: 100,
                },
                BikeWalkingAccess::new(0, 0),
            )
            .unwrap()
        };
        assert!(
            append_bike_share(
                &reconstructor,
                &context,
                &place(0),
                &place(3),
                &movement(0, 1, "1", "2"),
                &mut assembly
            )
            .await
            .unwrap()
        );
        assert_eq!(assembly.arrival_time, departure + Duration::seconds(360));
        let second = movement(1, 2, "4", "5").with_rental_wait(900).unwrap();
        assert!(
            append_bike_share(
                &reconstructor,
                &context,
                &place(3),
                &place(6),
                &second,
                &mut assembly
            )
            .await
            .unwrap()
        );
        assert_eq!(
            assembly.legs[3].departure_time,
            departure + Duration::seconds(960)
        );
        assert_eq!(assembly.arrival_time, departure + Duration::seconds(1020));
        assert_eq!(assembly.bike_share_uses, 2);
        assert_eq!(assembly.walking_seconds, 350);
        assert_eq!(assembly.bicycle_seconds, 70);
    }
}
