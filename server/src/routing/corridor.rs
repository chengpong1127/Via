use super::model::RoutingError;
use super::{
    model::PlanningQuery,
    realized::{JourneyAnchorKind, WalkingMovementKind},
    static_search::{StaticCorridor, StaticCorridorStep, StaticTransitLeg},
};
use crate::domain::ids::BoardingPointId;
use crate::{domain::transit_runtime::TransitRuntime, street::Coordinate};

pub(crate) struct EvaluationCorridor {
    anchors: Vec<CorridorAnchor>,
    edges: Vec<CorridorEdge>,
}

pub(crate) struct CorridorEdge {
    pub planned_movement: PlannedMovement,
}

pub(crate) enum PlannedMovement {
    Transit(StaticTransitLeg),
    WalkingToResolve,
    RetainedWalking {
        kind: WalkingMovementKind,
        duration_seconds: u32,
        distance_metres: u32,
        access_available_at: Option<time::OffsetDateTime>,
    },
}

impl EvaluationCorridor {
    pub(crate) fn new(anchors: Vec<CorridorAnchor>, edges: Vec<CorridorEdge>) -> Option<Self> {
        (anchors.len() >= 2 && edges.len() == anchors.len() - 1).then_some(Self { anchors, edges })
    }

    pub fn anchors(&self) -> &[CorridorAnchor] {
        &self.anchors
    }

    pub fn edges(&self) -> &[CorridorEdge] {
        &self.edges
    }

    pub fn direct_walking(origin: Coordinate, destination: Coordinate) -> Self {
        Self {
            anchors: vec![
                CorridorAnchor {
                    kind: JourneyAnchorKind::Origin,
                    coordinate: origin,
                },
                CorridorAnchor {
                    kind: JourneyAnchorKind::Destination,
                    coordinate: destination,
                },
            ],
            edges: vec![CorridorEdge {
                planned_movement: PlannedMovement::WalkingToResolve,
            }],
        }
    }

    pub fn from_transit(
        network: &TransitRuntime,
        retained: &StaticCorridor,
        query: &PlanningQuery,
    ) -> Result<Self, RoutingError> {
        let stops = retained
            .anchors()
            .ok_or_else(|| RoutingError::Internal("retained corridor has no anchors".into()))?
            .stops()
            .to_vec();
        let stop_anchors = stops
            .iter()
            .map(|id| {
                let stop = network.stop(id).ok_or_else(|| {
                    RoutingError::Internal(format!(
                        "retained corridor references unknown stop {id:?}"
                    ))
                })?;
                let coordinate = stop.coordinate.try_into().map_err(|_| {
                    RoutingError::Internal(format!("stop {id:?} has an invalid coordinate"))
                })?;
                Ok(CorridorAnchor {
                    kind: JourneyAnchorKind::BoardingPoint { id: id.clone() },
                    coordinate,
                })
            })
            .collect::<Result<Vec<_>, RoutingError>>()?;
        let origin = query.origin;
        let destination = query.destination;
        let mut anchors = vec![CorridorAnchor {
            kind: JourneyAnchorKind::Origin,
            coordinate: origin,
        }];
        anchors.extend(stop_anchors);
        anchors.push(CorridorAnchor {
            kind: JourneyAnchorKind::Destination,
            coordinate: destination,
        });
        let access = query
            .origin_access
            .iter()
            .find(|a| a.stop_id == retained.access_stop_id)
            .ok_or_else(|| {
                RoutingError::Internal("retained corridor has no matching access point".into())
            })?;
        let mut edges = vec![CorridorEdge {
            planned_movement: PlannedMovement::RetainedWalking {
                kind: WalkingMovementKind::Access,
                duration_seconds: access.walking_seconds,
                distance_metres: access.distance_metres,
                access_available_at: Some(access.available_at),
            },
        }];
        edges.extend(retained.steps.iter().map(|step| CorridorEdge {
            planned_movement: match step {
                StaticCorridorStep::Transit(leg) => PlannedMovement::Transit(leg.clone()),
                StaticCorridorStep::SurfaceTransfer {
                    walking_duration_seconds,
                    walking_distance_metres,
                    ..
                } => PlannedMovement::RetainedWalking {
                    kind: WalkingMovementKind::Transfer,
                    duration_seconds: *walking_duration_seconds,
                    distance_metres: *walking_distance_metres,
                    access_available_at: None,
                },
            },
        }));
        let egress = query
            .destination_egress
            .iter()
            .find(|e| e.stop_id == retained.egress_stop_id)
            .ok_or_else(|| {
                RoutingError::Internal("retained corridor has no matching egress point".into())
            })?;
        edges.push(CorridorEdge {
            planned_movement: PlannedMovement::RetainedWalking {
                kind: WalkingMovementKind::Egress,
                duration_seconds: egress.walking_seconds,
                distance_metres: egress.distance_metres,
                access_available_at: None,
            },
        });
        Self::new(anchors, edges).ok_or_else(|| {
            RoutingError::Internal("retained corridor has inconsistent anchors and edges".into())
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CorridorAnchor {
    pub kind: JourneyAnchorKind,
    pub coordinate: Coordinate,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CorridorAnchorKey {
    latitude: u64,
    longitude: u64,
    boarding_point: Option<BoardingPointId>,
}

impl CorridorAnchor {
    pub fn boarding_point(&self) -> Option<&BoardingPointId> {
        match &self.kind {
            JourneyAnchorKind::BoardingPoint { id } => Some(id),
            _ => None,
        }
    }
    pub fn cache_key(&self) -> CorridorAnchorKey {
        CorridorAnchorKey {
            latitude: self.coordinate.latitude().to_bits(),
            longitude: self.coordinate.longitude().to_bits(),
            boarding_point: self.boarding_point().cloned(),
        }
    }
}
