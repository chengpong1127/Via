use std::collections::HashMap;

use crate::domain::ids::BoardingPointId;
use crate::routing::{
    journey::{JourneyAnchorKind, WalkingMovementKind},
    model::{PlanningQuery, RoutingError},
};

use super::types::{EquivalentLegKey, StaticCorridorStep, StaticTransitLeg, equivalent_leg_key};
use crate::{domain::transit::runtime::TransitRuntime, street::Coordinate};

#[derive(Clone, Debug, PartialEq)]
pub struct Corridor {
    anchors: Vec<CorridorAnchor>,
    edges: Vec<PlannedMovement>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlannedMovement {
    Transit {
        legs: Vec<StaticTransitLeg>,
    },
    WalkingToResolve,
    RetainedWalking {
        kind: WalkingMovementKind,
        duration_seconds: u32,
        distance_metres: u32,
    },
}

impl Corridor {
    pub(crate) fn new(anchors: Vec<CorridorAnchor>, edges: Vec<PlannedMovement>) -> Option<Self> {
        (anchors.len() >= 2 && edges.len() == anchors.len() - 1).then_some(Self { anchors, edges })
    }

    pub fn anchors(&self) -> &[CorridorAnchor] {
        &self.anchors
    }

    pub fn edges(&self) -> &[PlannedMovement] {
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
            edges: vec![PlannedMovement::WalkingToResolve],
        }
    }

    pub(in crate::routing) fn from_steps(
        transit: &TransitRuntime,
        query: &PlanningQuery,
        access_stop_id: &BoardingPointId,
        steps: &[StaticCorridorStep],
        egress_stop_id: &BoardingPointId,
    ) -> Result<Self, RoutingError> {
        let mut stops = vec![access_stop_id.clone()];
        for step in steps {
            let (from, to) = match step {
                StaticCorridorStep::Transit(leg) => (&leg.board_stop_id, &leg.alight_stop_id),
                StaticCorridorStep::SurfaceTransfer { from, to, .. } => (from, to),
            };
            if stops.last() != Some(from) {
                return Err(RoutingError::Internal(
                    "retained corridor steps are not connected".into(),
                ));
            }
            stops.push(to.clone());
        }
        if stops.last() != Some(egress_stop_id) {
            return Err(RoutingError::Internal(
                "retained corridor steps do not reach the egress stop".into(),
            ));
        }
        let stop_anchors = stops
            .iter()
            .map(|id| {
                let stop = transit.stop(id).ok_or_else(|| {
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
        let mut anchors = vec![CorridorAnchor {
            kind: JourneyAnchorKind::Origin,
            coordinate: query.origin,
        }];
        anchors.extend(stop_anchors);
        anchors.push(CorridorAnchor {
            kind: JourneyAnchorKind::Destination,
            coordinate: query.destination,
        });
        let access = query
            .origin_access
            .iter()
            .find(|access| &access.stop_id == access_stop_id)
            .ok_or_else(|| {
                RoutingError::Internal("retained corridor has no matching access point".into())
            })?;
        let mut edges = vec![PlannedMovement::RetainedWalking {
            kind: WalkingMovementKind::Access,
            duration_seconds: access.walking_seconds,
            distance_metres: access.distance_metres,
        }];
        edges.extend(steps.iter().map(|step| match step {
            StaticCorridorStep::Transit(leg) => PlannedMovement::Transit {
                legs: vec![leg.clone()],
            },
            StaticCorridorStep::SurfaceTransfer {
                walking_duration_seconds,
                walking_distance_metres,
                ..
            } => PlannedMovement::RetainedWalking {
                kind: WalkingMovementKind::Transfer,
                duration_seconds: *walking_duration_seconds,
                distance_metres: *walking_distance_metres,
            },
        }));
        let egress = query
            .destination_egress
            .iter()
            .find(|egress| &egress.stop_id == egress_stop_id)
            .ok_or_else(|| {
                RoutingError::Internal("retained corridor has no matching egress point".into())
            })?;
        edges.push(PlannedMovement::RetainedWalking {
            kind: WalkingMovementKind::Egress,
            duration_seconds: egress.walking_seconds,
            distance_metres: egress.distance_metres,
        });
        Self::new(anchors, edges).ok_or_else(|| {
            RoutingError::Internal("retained corridor has inconsistent anchors and edges".into())
        })
    }

    pub(super) fn with_equivalent_legs(
        mut self,
        transit: &TransitRuntime,
        equivalent_legs_by_key: &HashMap<EquivalentLegKey, Vec<StaticTransitLeg>>,
    ) -> Self {
        for edge in &mut self.edges {
            let PlannedMovement::Transit { legs } = edge else {
                continue;
            };
            let Some(equivalent_legs) = legs
                .first()
                .and_then(|leg| equivalent_legs_by_key.get(&equivalent_leg_key(transit, leg)))
            else {
                continue;
            };
            legs.clone_from(equivalent_legs);
            legs.sort_by(|left, right| left.order_key().cmp(&right.order_key()));
        }
        self
    }

    pub fn transit_legs(&self) -> impl Iterator<Item = &StaticTransitLeg> {
        self.edges
            .iter()
            .filter_map(|edge| match edge {
                PlannedMovement::Transit { legs } => Some(legs.iter()),
                PlannedMovement::WalkingToResolve | PlannedMovement::RetainedWalking { .. } => None,
            })
            .flatten()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CorridorAnchor {
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
    pub(crate) fn cache_key(&self) -> CorridorAnchorKey {
        CorridorAnchorKey {
            latitude: self.coordinate.latitude().to_bits(),
            longitude: self.coordinate.longitude().to_bits(),
            boarding_point: self.boarding_point().cloned(),
        }
    }
}
