use time::{Duration, OffsetDateTime};

use crate::{
    domain::journey::{Geometry, Guidance, Leg, LegDetails, LegMode, Place, PlanningError},
    routing::{JourneyAnchorKind, JourneyPathStep, model::JourneyPlanningContext},
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
        step: &JourneyPathStep,
        shortcut: &crate::routing::BikeShortcut,
    ) -> Result<bool, PlanningError> {
        if shortcut.from_anchor != step.from_anchor || shortcut.to_anchor != step.to_anchor {
            return Ok(false);
        }
        if matches!(
            self.variant.skeleton().anchors()[step.from_anchor].kind,
            JourneyAnchorKind::Destination
        ) || matches!(
            self.variant.skeleton().anchors()[step.to_anchor].kind,
            JourneyAnchorKind::Origin
        ) {
            return Ok(false);
        }
        let source_stop = anchor_boarding_point(self.variant, step.from_anchor);
        if source_stop
            .as_ref()
            .is_some_and(|source_stop| self.current_stop.as_ref() != Some(source_stop))
        {
            return Ok(false);
        }
        let from = anchor_place(
            self.reconstructor,
            self.context.request,
            self.variant,
            step.from_anchor,
        )
        .ok_or(PlanningError::Internal)?;
        let to = anchor_place(
            self.reconstructor,
            self.context.request,
            self.variant,
            step.to_anchor,
        )
        .ok_or(PlanningError::Internal)?;
        let connector_start = self.result.legs.len();
        if !append_bike_share(
            self.reconstructor,
            self.context,
            &from,
            &to,
            shortcut,
            &mut self.result,
        )
        .await?
        {
            return Ok(false);
        }
        register_pending_transfer(&self.result, &mut self.pending_transfer, connector_start);
        if let Some(to_stop) = anchor_boarding_point(self.variant, step.to_anchor) {
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
    shortcut: &crate::routing::BikeShortcut,
    result: &mut JourneyAssembly,
) -> Result<bool, PlanningError> {
    let Some(pickup) = reconstructor.bike_station_place(&shortcut.pickup_station_id) else {
        return Ok(false);
    };
    let Some(dropoff) = reconstructor.bike_station_place(&shortcut.dropoff_station_id) else {
        return Ok(false);
    };

    let Some(to_pickup) = strict_bike_path(
        reconstructor,
        context,
        from,
        &pickup,
        StreetTravelMode::Walking,
        result.arrival_time,
    )
    .await
    else {
        return Ok(false);
    };
    append_routed_walking(from, &pickup, to_pickup, result);

    let Some(bicycle) = strict_bike_path(
        reconstructor,
        context,
        &pickup,
        &dropoff,
        StreetTravelMode::Bicycle,
        result.arrival_time,
    )
    .await
    else {
        return Ok(false);
    };
    append_bicycle(&pickup, &dropoff, bicycle, result);

    let Some(from_dropoff) = strict_bike_path(
        reconstructor,
        context,
        &dropoff,
        to,
        StreetTravelMode::Walking,
        result.arrival_time,
    )
    .await
    else {
        return Ok(false);
    };
    append_routed_walking(&dropoff, to, from_dropoff, result);
    result.bike_share_uses = result.bike_share_uses.saturating_add(1);
    Ok(true)
}

async fn strict_bike_path(
    reconstructor: &JourneyReconstructor,
    context: &JourneyPlanningContext<'_>,
    from: &Place,
    to: &Place,
    mode: StreetTravelMode,
    departure: OffsetDateTime,
) -> Option<StreetPath> {
    hydrate_path(reconstructor, context, from, to, mode, departure)
        .await
        .unwrap_or_default()
}

fn append_bicycle(pickup: &Place, dropoff: &Place, path: StreetPath, result: &mut JourneyAssembly) {
    let departure = result.arrival_time;
    let arrival = departure + Duration::seconds(i64::from(path.duration.0));
    let leg_id = format!("candidate_leg_{}", result.legs.len());
    result.legs.push(Leg {
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
            pickup_station_id: pickup.via_id.clone().unwrap_or_default(),
            pickup_station_name: pickup.display_name.clone(),
            dropoff_station_id: dropoff.via_id.clone().unwrap_or_default(),
            dropoff_station_name: dropoff.display_name.clone(),
        },
        attribution: path.attribution.clone(),
    });
    result.bicycle_seconds = result.bicycle_seconds.saturating_add(path.duration.0);
    result.arrival_time = arrival;
    evidence::push_unique_mode(&mut result.travel_modes, LegMode::Bicycle);
}
