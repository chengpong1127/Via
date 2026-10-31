use super::*;

impl RealizedJourney {
    pub(in crate::routing) fn signature(&self) -> String {
        if !self.has_bike_share() {
            return self
                .movements
                .iter()
                .map(|movement| self.baseline_signature(movement))
                .collect::<Vec<_>>()
                .join("|");
        }
        let parts = self
            .movements
            .iter()
            .map(|movement| match &movement.kind {
                JourneyMovementKind::BikeShare { movement: bike } => format!(
                    "bike:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                    self.anchor(movement.from_anchor),
                    self.anchor(movement.to_anchor),
                    bike.pickup_station_id.as_str(),
                    bike.dropoff_station_id.as_str(),
                    bike.walk_to_pickup.duration_seconds,
                    bike.walk_to_pickup.distance_metres,
                    bike.bicycle_duration_seconds,
                    bike.bicycle_distance_metres,
                    bike.walk_from_dropoff.duration_seconds,
                    bike.walk_from_dropoff.distance_metres,
                    bike.rental_wait_seconds,
                    bike.total_seconds
                ),
                _ => format!("baseline:{}", self.movement_signature(movement)),
            })
            .collect::<Vec<_>>();
        format!("path:v2:{}", parts.join("|"))
    }

    fn baseline_signature(&self, movement: &JourneyMovement) -> String {
        match &movement.kind {
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Access,
                duration_seconds,
                ..
            } => {
                let stop = match &self.anchors[movement.to_anchor].kind {
                    JourneyAnchorKind::BoardingPoint { id } => id.as_str(),
                    _ => "invalid",
                };
                format!("access:{stop}:{duration_seconds}")
            }
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Transfer,
                duration_seconds,
                ..
            } => format!(
                "transfer:{}:{}:{duration_seconds}",
                self.unprefixed_anchor(movement.from_anchor),
                self.unprefixed_anchor(movement.to_anchor)
            ),
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Egress,
                duration_seconds,
                ..
            } => format!(
                "egress:{}:{duration_seconds}",
                self.unprefixed_anchor(movement.from_anchor)
            ),
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Direct,
                duration_seconds,
                distance_metres,
            } => format!(
                "direct-walking:{}:{duration_seconds}:{distance_metres}:{}",
                self.departure_at.unix_timestamp_nanos(),
                self.anchor(movement.to_anchor)
            ),
            JourneyMovementKind::Transit {
                trip,
                board_index,
                alight_index,
            } => match trip.stop_times.get(*board_index) {
                Some(board) => format!(
                    "transit:{}:{board_index}:{alight_index}:{}",
                    trip.id.as_str(),
                    board.departure_at.unix_timestamp_nanos()
                ),
                None => format!("invalid-transit:{board_index}"),
            },
            JourneyMovementKind::BikeShare { .. } => {
                unreachable!("bike paths use path signatures")
            }
        }
    }

    fn movement_signature(&self, movement: &JourneyMovement) -> String {
        match &movement.kind {
            JourneyMovementKind::Walking {
                kind,
                duration_seconds,
                distance_metres,
            } => format!(
                "walking:{kind:?}:{}:{}:{duration_seconds}:{distance_metres}",
                self.anchor(movement.from_anchor),
                self.anchor(movement.to_anchor)
            ),
            JourneyMovementKind::Transit {
                trip,
                board_index,
                alight_index,
            } => {
                let Some(board) = trip.stop_times.get(*board_index) else {
                    return format!("invalid-transit:{board_index}");
                };
                let Some(alight) = trip.stop_times.get(*alight_index) else {
                    return format!("invalid-transit:{alight_index}");
                };
                format!(
                    "transit:{}:{}:{}:{}:{}:{}:{}:{}",
                    trip.id.as_str(),
                    trip.pattern_id.as_str(),
                    board.stop_id.as_str(),
                    alight.stop_id.as_str(),
                    board.departure_at.unix_timestamp_nanos(),
                    alight.arrival_at.unix_timestamp_nanos(),
                    self.anchor(movement.from_anchor),
                    self.anchor(movement.to_anchor)
                )
            }
            JourneyMovementKind::BikeShare { .. } => {
                unreachable!("bike movements have their own signature")
            }
        }
    }

    fn anchor(&self, index: usize) -> String {
        match &self.anchors[index].kind {
            JourneyAnchorKind::Origin => "origin".to_owned(),
            JourneyAnchorKind::BoardingPoint { id } => format!("boarding_point:{}", id.as_str()),
            JourneyAnchorKind::Destination => "destination".to_owned(),
        }
    }

    fn unprefixed_anchor(&self, index: usize) -> &str {
        match &self.anchors[index].kind {
            JourneyAnchorKind::Origin => "origin",
            JourneyAnchorKind::BoardingPoint { id } => {
                id.as_str().trim_start_matches("boarding_point:")
            }
            JourneyAnchorKind::Destination => "destination",
        }
    }
}
