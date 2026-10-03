//! Phase 2 anchors and baselines, independent of static transit discovery.
use super::{
    model::TransitQuery,
    static_search::{StaticCorridor, StaticCorridorStep, StaticTransitLeg},
    surface::SurfaceEndpoint,
    variant::{JourneyAnchorKind, WalkingMovementKind},
};
use crate::{domain::transit_runtime::TransitRuntime, street::Coordinate};

pub(crate) struct EvaluationCorridor {
    pub anchors: Vec<SurfaceEndpoint>,
    pub edges: Vec<CorridorEdge>,
}

pub(crate) struct CorridorEdge {
    pub baseline: Vec<BaselineMovement>,
}

pub(crate) enum BaselineMovement {
    Transit(StaticTransitLeg),
    Walking {
        kind: WalkingMovementKind,
        duration_seconds: u32,
        distance_metres: u32,
        ready_at: Option<time::OffsetDateTime>,
    },
}

impl EvaluationCorridor {
    pub fn trivial(origin: Coordinate, destination: Coordinate) -> Self {
        Self {
            anchors: vec![
                SurfaceEndpoint {
                    kind: JourneyAnchorKind::Origin,
                    coordinate: origin,
                },
                SurfaceEndpoint {
                    kind: JourneyAnchorKind::Destination,
                    coordinate: destination,
                },
            ],
            edges: vec![CorridorEdge {
                baseline: Vec::new(),
            }],
        }
    }

    pub fn from_transit(
        network: &TransitRuntime,
        retained: &StaticCorridor,
        query: &TransitQuery,
    ) -> Option<Self> {
        let stops = retained.anchors()?.stops().to_vec();
        let stop_anchors = stops
            .iter()
            .map(|id| {
                Some(SurfaceEndpoint {
                    kind: JourneyAnchorKind::BoardingPoint { id: id.clone() },
                    coordinate: network.stop(id)?.coordinate.try_into().ok()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        // Legacy transit queries omit geographic endpoints; the public service supplies them.
        let origin = query.origin.unwrap_or(stop_anchors.first()?.coordinate);
        let destination = query.destination.unwrap_or(stop_anchors.last()?.coordinate);
        let mut anchors = vec![SurfaceEndpoint {
            kind: JourneyAnchorKind::Origin,
            coordinate: origin,
        }];
        anchors.extend(stop_anchors);
        anchors.push(SurfaceEndpoint {
            kind: JourneyAnchorKind::Destination,
            coordinate: destination,
        });
        let mut edges = vec![CorridorEdge {
            baseline: query
                .origin_access
                .iter()
                .filter(|a| a.stop_id == retained.access_stop_id)
                .map(|a| BaselineMovement::Walking {
                    kind: WalkingMovementKind::Access,
                    duration_seconds: a.walking_seconds,
                    distance_metres: a.distance_metres,
                    ready_at: Some(a.available_at),
                })
                .collect(),
        }];
        if edges[0].baseline.is_empty() {
            return None;
        }
        edges.extend(retained.steps.iter().map(|step| CorridorEdge {
            baseline: vec![match step {
                StaticCorridorStep::Transit(leg) => BaselineMovement::Transit(leg.clone()),
                StaticCorridorStep::SurfaceTransfer {
                    walking_duration_seconds,
                    walking_distance_metres,
                    ..
                } => BaselineMovement::Walking {
                    kind: WalkingMovementKind::Transfer,
                    duration_seconds: *walking_duration_seconds,
                    distance_metres: *walking_distance_metres,
                    ready_at: None,
                },
            }],
        }));
        let egress = query
            .destination_egress
            .iter()
            .filter(|e| e.stop_id == retained.egress_stop_id)
            .map(|e| BaselineMovement::Walking {
                kind: WalkingMovementKind::Egress,
                duration_seconds: e.walking_seconds,
                distance_metres: e.distance_metres,
                ready_at: None,
            })
            .collect::<Vec<_>>();
        if egress.is_empty() {
            return None;
        }
        edges.push(CorridorEdge { baseline: egress });
        Some(Self { anchors, edges })
    }
}
