use crate::domain::ids::{BikeStationId, BoardingPointId, EffectiveTripId, TransitPatternId};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::routing) struct JourneyVariantIdentity {
    is_baseline: bool,
    movements: Vec<JourneyVariantMovementIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum JourneyVariantMovementIdentity {
    Baseline(Box<JourneyVariantMovementIdentity>),
    Access {
        stop_id: Option<BoardingPointId>,
        duration_seconds: u32,
    },
    Transfer {
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
        duration_seconds: u32,
    },
    Egress {
        from: JourneyAnchorIdentity,
        duration_seconds: u32,
    },
    DirectWalking {
        departure_unix_nanos: i128,
        duration_seconds: u32,
        distance_metres: u32,
        to: JourneyAnchorIdentity,
    },
    Walking {
        kind: WalkingMovementKind,
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
        duration_seconds: u32,
        distance_metres: u32,
    },
    Transit {
        trip_id: EffectiveTripId,
        board_index: usize,
        alight_index: usize,
        departure_unix_nanos: i128,
    },
    TransitMovement {
        trip_id: EffectiveTripId,
        pattern_id: TransitPatternId,
        board_stop_id: BoardingPointId,
        alight_stop_id: BoardingPointId,
        departure_unix_nanos: i128,
        arrival_unix_nanos: i128,
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
    },
    BikeShare {
        from: JourneyAnchorIdentity,
        to: JourneyAnchorIdentity,
        pickup_station_id: BikeStationId,
        dropoff_station_id: BikeStationId,
        walk_to_pickup_seconds: u32,
        walk_to_pickup_metres: u32,
        bicycle_duration_seconds: u32,
        bicycle_distance_metres: u32,
        walk_from_dropoff_seconds: u32,
        walk_from_dropoff_metres: u32,
        rental_wait_seconds: u32,
        total_seconds: u32,
    },
    InvalidMovement(usize),
    InvalidTransit(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum JourneyAnchorIdentity {
    Origin,
    BoardingPoint(BoardingPointId),
    Destination,
}

impl JourneyAnchorIdentity {
    fn from_kind(kind: &JourneyAnchorKind) -> Self {
        match kind {
            JourneyAnchorKind::Origin => Self::Origin,
            JourneyAnchorKind::BoardingPoint { id } => Self::BoardingPoint(id.clone()),
            JourneyAnchorKind::Destination => Self::Destination,
        }
    }

    fn legacy_signature(&self) -> String {
        match self {
            Self::Origin => "origin".to_owned(),
            Self::BoardingPoint(id) => format!("boarding_point:{}", id.as_str()),
            Self::Destination => "destination".to_owned(),
        }
    }

    fn boarding_point_id(&self) -> Option<&BoardingPointId> {
        match self {
            Self::BoardingPoint(id) => Some(id),
            Self::Origin | Self::Destination => None,
        }
    }

    fn legacy_unprefixed(&self) -> &str {
        match self {
            Self::Origin => "origin",
            Self::BoardingPoint(id) => id.as_str().trim_start_matches("boarding_point:"),
            Self::Destination => "destination",
        }
    }
}

impl JourneyVariantIdentity {
    pub(in crate::routing) fn canonical_signature(&self) -> String {
        let parts = self
            .movements
            .iter()
            .map(JourneyVariantMovementIdentity::legacy_signature)
            .collect::<Vec<_>>()
            .join("|");
        if self.is_baseline {
            parts
        } else {
            format!("path:v2:{parts}")
        }
    }
}

impl JourneyVariantMovementIdentity {
    fn legacy_signature(&self) -> String {
        match self {
            Self::Baseline(movement) => format!("baseline:{}", movement.legacy_signature()),
            Self::Access {
                stop_id,
                duration_seconds,
            } => format!(
                "access:{}:{duration_seconds}",
                stop_id.as_ref().map_or("invalid", BoardingPointId::as_str)
            ),
            Self::Transfer {
                from,
                to,
                duration_seconds,
            } => format!(
                "transfer:{}:{}:{duration_seconds}",
                from.legacy_unprefixed(),
                to.legacy_unprefixed()
            ),
            Self::Egress {
                from,
                duration_seconds,
            } => format!("egress:{}:{duration_seconds}", from.legacy_unprefixed()),
            Self::DirectWalking {
                departure_unix_nanos,
                duration_seconds,
                distance_metres,
                to,
            } => format!(
                "direct-walking:{departure_unix_nanos}:{duration_seconds}:{distance_metres}:{}",
                to.legacy_signature()
            ),
            Self::Walking {
                kind,
                from,
                to,
                duration_seconds,
                distance_metres,
            } => format!(
                "walking:{kind:?}:{}:{}:{duration_seconds}:{distance_metres}",
                from.legacy_signature(),
                to.legacy_signature()
            ),
            Self::Transit {
                trip_id,
                board_index,
                alight_index,
                departure_unix_nanos,
            } => format!(
                "transit:{}:{board_index}:{alight_index}:{departure_unix_nanos}",
                trip_id.as_str()
            ),
            Self::TransitMovement {
                trip_id,
                pattern_id,
                board_stop_id,
                alight_stop_id,
                departure_unix_nanos,
                arrival_unix_nanos,
                from,
                to,
            } => format!(
                "transit:{}:{}:{}:{}:{departure_unix_nanos}:{arrival_unix_nanos}:{}:{}",
                trip_id.as_str(),
                pattern_id.as_str(),
                board_stop_id.as_str(),
                alight_stop_id.as_str(),
                from.legacy_signature(),
                to.legacy_signature()
            ),
            Self::BikeShare {
                from,
                to,
                pickup_station_id,
                dropoff_station_id,
                walk_to_pickup_seconds,
                walk_to_pickup_metres,
                bicycle_duration_seconds,
                bicycle_distance_metres,
                walk_from_dropoff_seconds,
                walk_from_dropoff_metres,
                rental_wait_seconds,
                total_seconds,
            } => format!(
                "bike:{}:{}:{}:{}:{walk_to_pickup_seconds}:{walk_to_pickup_metres}:{bicycle_duration_seconds}:{bicycle_distance_metres}:{walk_from_dropoff_seconds}:{walk_from_dropoff_metres}:{rental_wait_seconds}:{total_seconds}",
                from.legacy_signature(),
                to.legacy_signature(),
                pickup_station_id.as_str(),
                dropoff_station_id.as_str()
            ),
            Self::InvalidMovement(index) => format!("invalid:{index}"),
            Self::InvalidTransit(index) => format!("invalid-transit:{index}"),
        }
    }
}

impl JourneyVariant {
    pub(in crate::routing) fn identity(&self) -> JourneyVariantIdentity {
        let is_baseline = self.is_baseline();
        let movements = if is_baseline {
            self.skeleton.baseline_identity()
        } else {
            self.path
                .iter()
                .map(|step| match &step.movement {
                    JourneyPathMovement::Walking { movement_index }
                    | JourneyPathMovement::Transit { movement_index } => {
                        JourneyVariantMovementIdentity::Baseline(Box::new(
                            self.skeleton.baseline_movement_identity(*movement_index),
                        ))
                    }
                    JourneyPathMovement::BikeShare { shortcut } => {
                        JourneyVariantMovementIdentity::BikeShare {
                            from: JourneyAnchorIdentity::from_kind(
                                &self.skeleton.anchors()[step.from_anchor].kind,
                            ),
                            to: JourneyAnchorIdentity::from_kind(
                                &self.skeleton.anchors()[step.to_anchor].kind,
                            ),
                            pickup_station_id: shortcut.pickup_station_id.clone(),
                            dropoff_station_id: shortcut.dropoff_station_id.clone(),
                            walk_to_pickup_seconds: shortcut.walk_to_pickup.duration_seconds,
                            walk_to_pickup_metres: shortcut.walk_to_pickup.distance_metres,
                            bicycle_duration_seconds: shortcut.bicycle_duration_seconds,
                            bicycle_distance_metres: shortcut.bicycle_distance_metres,
                            walk_from_dropoff_seconds: shortcut.walk_from_dropoff.duration_seconds,
                            walk_from_dropoff_metres: shortcut.walk_from_dropoff.distance_metres,
                            rental_wait_seconds: shortcut.rental_wait_seconds,
                            total_seconds: shortcut.total_seconds,
                        }
                    }
                })
                .collect()
        };
        JourneyVariantIdentity {
            is_baseline,
            movements,
        }
    }
}

impl JourneySkeleton {
    fn baseline_movement_identity(&self, movement_index: usize) -> JourneyVariantMovementIdentity {
        let Some(movement) = self.movements.get(movement_index) else {
            return JourneyVariantMovementIdentity::InvalidMovement(movement_index);
        };
        let from = JourneyAnchorIdentity::from_kind(&self.anchors[movement.from_anchor].kind);
        let to = JourneyAnchorIdentity::from_kind(&self.anchors[movement.to_anchor].kind);
        match &movement.kind {
            JourneyMovementKind::BikeShare { .. } => {
                JourneyVariantMovementIdentity::InvalidMovement(movement_index)
            }
            JourneyMovementKind::Walking {
                kind,
                duration_seconds,
                distance_metres,
            } => JourneyVariantMovementIdentity::Walking {
                kind: *kind,
                from,
                to,
                duration_seconds: *duration_seconds,
                distance_metres: *distance_metres,
            },
            JourneyMovementKind::Transit {
                trip,
                board_index,
                alight_index,
            } => {
                let Some(board) = trip.stop_times.get(*board_index) else {
                    return JourneyVariantMovementIdentity::InvalidTransit(*board_index);
                };
                let Some(alight) = trip.stop_times.get(*alight_index) else {
                    return JourneyVariantMovementIdentity::InvalidTransit(*alight_index);
                };
                JourneyVariantMovementIdentity::TransitMovement {
                    trip_id: trip.id.clone(),
                    pattern_id: trip.pattern_id.clone(),
                    board_stop_id: board.stop_id.clone(),
                    alight_stop_id: alight.stop_id.clone(),
                    departure_unix_nanos: board.departure_at.unix_timestamp_nanos(),
                    arrival_unix_nanos: alight.arrival_at.unix_timestamp_nanos(),
                    from,
                    to,
                }
            }
        }
    }

    fn baseline_identity(&self) -> Vec<JourneyVariantMovementIdentity> {
        let mut identities = Vec::with_capacity(self.movements.len());
        for movement in &self.movements {
            let from = JourneyAnchorIdentity::from_kind(&self.anchors[movement.from_anchor].kind);
            let to = JourneyAnchorIdentity::from_kind(&self.anchors[movement.to_anchor].kind);
            match &movement.kind {
                JourneyMovementKind::BikeShare { .. } => {
                    unreachable!("bike paths use path identities")
                }
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Access,
                    duration_seconds,
                    ..
                } => identities.push(JourneyVariantMovementIdentity::Access {
                    stop_id: to.boarding_point_id().cloned(),
                    duration_seconds: *duration_seconds,
                }),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Transfer,
                    duration_seconds,
                    ..
                } => identities.push(JourneyVariantMovementIdentity::Transfer {
                    from,
                    to,
                    duration_seconds: *duration_seconds,
                }),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Egress,
                    duration_seconds,
                    ..
                } => identities.push(JourneyVariantMovementIdentity::Egress {
                    from,
                    duration_seconds: *duration_seconds,
                }),
                JourneyMovementKind::Walking {
                    kind: WalkingMovementKind::Direct,
                    duration_seconds,
                    distance_metres,
                } => identities.push(JourneyVariantMovementIdentity::DirectWalking {
                    departure_unix_nanos: self.departure_at.unix_timestamp_nanos(),
                    duration_seconds: *duration_seconds,
                    distance_metres: *distance_metres,
                    to,
                }),
                JourneyMovementKind::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => {
                    let Some(board) = trip.stop_times.get(*board_index) else {
                        identities
                            .push(JourneyVariantMovementIdentity::InvalidTransit(*board_index));
                        continue;
                    };
                    identities.push(JourneyVariantMovementIdentity::Transit {
                        trip_id: trip.id.clone(),
                        board_index: *board_index,
                        alight_index: *alight_index,
                        departure_unix_nanos: board.departure_at.unix_timestamp_nanos(),
                    });
                }
            }
        }
        identities
    }
}
