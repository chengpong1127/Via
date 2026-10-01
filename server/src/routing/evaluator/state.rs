use super::*;

#[derive(Clone)]
pub(super) struct EvaluationState {
    pub(super) stop_id: BoardingPointId,
    pub(super) ready_at: OffsetDateTime,
    pub(super) transit_legs: usize,
    pub(super) last_was_transit: bool,
    pub(super) active_bike_run: Option<ActiveBikeRun>,
    pub(super) bike_reuse_available_at: Option<OffsetDateTime>,
    pub(super) path: EvaluationPath,
}

#[derive(Clone)]
pub(super) struct ActiveBikeRun {
    pub(super) start_anchor: usize,
    pub(super) start_stop_id: BoardingPointId,
    pub(super) started_at: OffsetDateTime,
    pub(super) prefix_path: EvaluationPath,
    pub(super) shortcut: BikeShortcut,
}

#[cfg(test)]
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct CorridorMovement {
    pub(super) from_anchor: usize,
    pub(super) to_anchor: usize,
    pub(super) from: BoardingPointId,
    pub(super) to: BoardingPointId,
}

#[cfg(test)]
pub(super) type TestBikeRoutes = HashMap<CorridorMovement, Vec<BikeShortcut>>;

impl EvaluationState {
    pub(super) fn after_normal_transition(
        &self,
        stop_id: BoardingPointId,
        ready_at: OffsetDateTime,
        transit_legs: usize,
        last_was_transit: bool,
        path: EvaluationPath,
    ) -> Self {
        let mut bike_reuse_available_at = self.bike_reuse_available_at;
        if let Some(run) = &self.active_bike_run {
            let return_seconds = run
                .shortcut
                .walk_to_pickup
                .duration_seconds
                .saturating_add(run.shortcut.bicycle_duration_seconds);
            let reuse_at = run.started_at
                + Duration::seconds(i64::from(return_seconds))
                + Duration::minutes(15);
            bike_reuse_available_at =
                Some(bike_reuse_available_at.map_or(reuse_at, |known| known.max(reuse_at)));
        }
        Self {
            stop_id,
            ready_at,
            transit_legs,
            last_was_transit,
            active_bike_run: None,
            bike_reuse_available_at,
            path,
        }
    }
}

pub(super) fn bike_start(
    state: &EvaluationState,
    current_anchor: usize,
) -> Option<(usize, BoardingPointId, OffsetDateTime, EvaluationPath)> {
    if let Some(run) = &state.active_bike_run {
        return Some((
            run.start_anchor,
            run.start_stop_id.clone(),
            run.started_at,
            run.prefix_path.clone(),
        ));
    }
    let started_at = state
        .bike_reuse_available_at
        .map_or(state.ready_at, |available| available.max(state.ready_at));
    Some((
        current_anchor,
        state.stop_id.clone(),
        started_at,
        state.path.clone(),
    ))
}

#[derive(Clone)]
pub(super) struct EvaluationPath {
    pub(super) access: AccessPoint,
    pub(super) segments: Vec<JourneySegment>,
    pub(super) walking_seconds: u32,
    pub(super) evidence: Vec<NormalizedRealtimeValue>,
    pub(super) timing_bases: Vec<TimingBasis>,
    pub(super) timing_reliability: TimingReliability,
    pub(super) uncertainty_seconds: Option<u32>,
    pub(super) bike_shortcuts: Vec<(usize, BikeShortcut)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct EvaluationPathIdentity {
    segments: Vec<EvaluationSegmentIdentity>,
    bike_shortcuts: Vec<EvaluationBikeIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum EvaluationSegmentIdentity {
    Transit {
        trip_id: EffectiveTripId,
        board_index: usize,
        alight_index: usize,
    },
    WalkingTransfer {
        from: BoardingPointId,
        to: BoardingPointId,
        duration_seconds: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EvaluationBikeIdentity {
    segment_index: usize,
    pickup_station_id: BikeStationId,
    dropoff_station_id: BikeStationId,
}

impl EvaluationPath {
    pub(super) fn from_access(access: AccessPoint) -> Self {
        Self {
            walking_seconds: access.walking_seconds,
            access,
            segments: Vec::new(),
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
            uncertainty_seconds: None,
            bike_shortcuts: Vec::new(),
        }
    }

    pub(super) fn with_ride(
        &self,
        trip: &EffectiveTrip,
        board_index: usize,
        alight_index: usize,
    ) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::Transit {
            trip: trip.clone(),
            board_index,
            alight_index,
        });
        next.evidence.extend(trip.evidence.clone());
        next.timing_bases.push(trip.timing_basis);
        if trip.timing_reliability == TimingReliability::Fallback {
            next.timing_reliability = TimingReliability::Fallback;
        }
        next.uncertainty_seconds = match (next.uncertainty_seconds, trip.uncertainty_seconds) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (None, value) | (value, None) => value,
        };
        next
    }

    pub(super) fn with_walking_transfer(&self, shortcut: &TransferShortcut) -> Self {
        let mut next = self.clone();
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: shortcut.clone(),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.duration_seconds);
        next
    }

    pub(super) fn with_bike_transfer(
        &self,
        from: &BoardingPointId,
        to: &BoardingPointId,
        shortcut: &BikeShortcut,
    ) -> Self {
        let mut next = self.clone();
        let segment_index = next.segments.len();
        // This skeleton placeholder is replaced by the exact BikeShare movement.
        // Its total duration includes cycling, so it has no walking distance.
        next.segments.push(JourneySegment::WalkingTransfer {
            shortcut: TransferShortcut::new(from.clone(), to.clone(), shortcut.total_seconds, 0),
        });
        next.walking_seconds = next
            .walking_seconds
            .saturating_add(shortcut.walk_to_pickup.duration_seconds)
            .saturating_add(shortcut.walk_from_dropoff.duration_seconds);
        next.bike_shortcuts.push((segment_index, shortcut.clone()));
        next
    }

    pub(super) fn identity(&self) -> EvaluationPathIdentity {
        let segments = self
            .segments
            .iter()
            .map(|segment| match segment {
                JourneySegment::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => EvaluationSegmentIdentity::Transit {
                    trip_id: trip.id.clone(),
                    board_index: *board_index,
                    alight_index: *alight_index,
                },
                JourneySegment::WalkingTransfer { shortcut } => {
                    EvaluationSegmentIdentity::WalkingTransfer {
                        from: shortcut.from.clone(),
                        to: shortcut.to.clone(),
                        duration_seconds: shortcut.duration_seconds,
                    }
                }
            })
            .collect();
        let bike_shortcuts = self
            .bike_shortcuts
            .iter()
            .map(|(index, shortcut)| EvaluationBikeIdentity {
                segment_index: *index,
                pickup_station_id: shortcut.pickup_station_id.clone(),
                dropoff_station_id: shortcut.dropoff_station_id.clone(),
            })
            .collect();
        EvaluationPathIdentity {
            segments,
            bike_shortcuts,
        }
    }

    // Keep the previous lexical tie order while typed identity handles equality.
    pub(super) fn deterministic_order_key(&self) -> String {
        let segments = self
            .segments
            .iter()
            .map(|segment| match segment {
                JourneySegment::Transit {
                    trip,
                    board_index,
                    alight_index,
                } => format!("transit:{}:{board_index}:{alight_index}", trip.id.as_str()),
                JourneySegment::WalkingTransfer { shortcut } => format!(
                    "transfer:{}:{}:{}",
                    shortcut.from.as_str(),
                    shortcut.to.as_str(),
                    shortcut.duration_seconds
                ),
            })
            .collect::<Vec<_>>()
            .join("|");
        let bikes = self
            .bike_shortcuts
            .iter()
            .map(|(index, shortcut)| {
                format!(
                    "bike:{index}:{}:{}",
                    shortcut.pickup_station_id.as_str(),
                    shortcut.dropoff_station_id.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        format!("{segments}|{bikes}")
    }

    pub(super) fn walking_seconds(&self) -> u32 {
        self.walking_seconds
    }

    pub(super) fn bicycle_seconds(&self) -> u32 {
        self.bike_shortcuts
            .iter()
            .map(|(_, shortcut)| shortcut.bicycle_duration_seconds)
            .fold(0, u32::saturating_add)
    }

    pub(super) fn same_timing_quality(&self, other: &Self) -> bool {
        self.timing_reliability == other.timing_reliability
            && super::pareto::same_timing_bases(&self.timing_bases, &other.timing_bases)
            && self.evidence == other.evidence
    }
}
