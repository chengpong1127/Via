//! Phase 2 anchors and baselines, independent of static transit discovery.
use super::model::RoutingError;
use super::{
    model::PlanningQuery,
    realized::{JourneyAnchorKind, WalkingMovementKind},
    static_search::{StaticCorridor, StaticCorridorStep, StaticTransitLeg},
    surface::CorridorAnchor,
};
use crate::{domain::transit_runtime::TransitRuntime, street::Coordinate};

pub(crate) struct EvaluationCorridor {
    anchors: Vec<CorridorAnchor>,
    edges: Vec<CorridorEdge>,
}

pub(crate) struct CorridorEdge {
    pub baseline: Vec<BaselineMovement>,
}

pub(crate) enum BaselineMovement {
    Transit(StaticTransitLeg),
    RetainedWalking {
        kind: WalkingMovementKind,
        duration_seconds: u32,
        distance_metres: u32,
        // Access discovery may retain an absolute ready time; transfer and egress derive it.
        access_available_at: Option<time::OffsetDateTime>,
    },
}

impl EvaluationCorridor {
    fn new(anchors: Vec<CorridorAnchor>, edges: Vec<CorridorEdge>) -> Option<Self> {
        (anchors.len() >= 2 && edges.len() == anchors.len() - 1).then_some(Self { anchors, edges })
    }

    pub fn anchors(&self) -> &[CorridorAnchor] {
        &self.anchors
    }

    pub fn edges(&self) -> &[CorridorEdge] {
        &self.edges
    }

    pub fn surface_only(origin: Coordinate, destination: Coordinate) -> Self {
        Self::new(
            vec![
                CorridorAnchor {
                    kind: JourneyAnchorKind::Origin,
                    coordinate: origin,
                },
                CorridorAnchor {
                    kind: JourneyAnchorKind::Destination,
                    coordinate: destination,
                },
            ],
            vec![CorridorEdge {
                baseline: Vec::new(),
            }],
        )
        .expect("surface-only corridor has one edge per anchor pair")
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
        if stops.is_empty() {
            return Err(RoutingError::Internal(
                "retained corridor has no stop anchors".into(),
            ));
        }
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
        let mut edges = vec![CorridorEdge {
            baseline: query
                .origin_access
                .iter()
                .filter(|a| a.stop_id == retained.access_stop_id)
                .map(|a| BaselineMovement::RetainedWalking {
                    kind: WalkingMovementKind::Access,
                    duration_seconds: a.walking_seconds,
                    distance_metres: a.distance_metres,
                    access_available_at: Some(a.available_at),
                })
                .collect(),
        }];
        if edges[0].baseline.is_empty() {
            return Err(RoutingError::Internal(
                "retained corridor has no matching access point".into(),
            ));
        }
        edges.extend(retained.steps.iter().map(|step| CorridorEdge {
            baseline: vec![match step {
                StaticCorridorStep::Transit(leg) => BaselineMovement::Transit(leg.clone()),
                StaticCorridorStep::SurfaceTransfer {
                    walking_duration_seconds,
                    walking_distance_metres,
                    ..
                } => BaselineMovement::RetainedWalking {
                    kind: WalkingMovementKind::Transfer,
                    duration_seconds: *walking_duration_seconds,
                    distance_metres: *walking_distance_metres,
                    access_available_at: None,
                },
            }],
        }));
        let egress = query
            .destination_egress
            .iter()
            .filter(|e| e.stop_id == retained.egress_stop_id)
            .map(|e| BaselineMovement::RetainedWalking {
                kind: WalkingMovementKind::Egress,
                duration_seconds: e.walking_seconds,
                distance_metres: e.distance_metres,
                access_available_at: None,
            })
            .collect::<Vec<_>>();
        if egress.is_empty() {
            return Err(RoutingError::Internal(
                "retained corridor has no matching egress point".into(),
            ));
        }
        edges.push(CorridorEdge { baseline: egress });
        Self::new(anchors, edges).ok_or_else(|| {
            RoutingError::Internal("retained corridor has inconsistent anchors and edges".into())
        })
    }
}
