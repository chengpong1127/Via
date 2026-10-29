use super::*;

#[derive(Clone)]
pub(super) struct EvaluationState {
    pub ready_at: OffsetDateTime,
    pub last_was_transit: bool,
    pub active_bike_rental: Option<ActiveBikeRental>,
    pub bike_reuse_available_at: Option<OffsetDateTime>,
    pub movements: Vec<JourneyMovement>,
}

impl EvaluationState {
    pub fn initial(query: &PlanningQuery) -> Self {
        Self {
            ready_at: query.departure_time,
            last_was_transit: false,
            active_bike_rental: None,
            bike_reuse_available_at: None,
            movements: Vec::new(),
        }
    }

    pub fn after_movement(&self, movement: JourneyMovement) -> Self {
        let mut next = self.clone();
        if let Some(rental) = &self.active_bike_rental {
            let return_at = rental.reuse_available_at();
            next.bike_reuse_available_at = Some(
                next.bike_reuse_available_at
                    .map_or(return_at, |known| known.max(return_at)),
            );
        }
        next.active_bike_rental = None;
        next.ready_at = movement.arrival_at;
        next.last_was_transit = matches!(movement.kind, JourneyMovementKind::Transit { .. });
        next.movements.push(movement);
        next
    }

    pub fn metrics(&self) -> PathMetrics {
        PathMetrics::from_movements(&self.movements, self.ready_at)
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub(super) struct PathMetrics {
    pub arrival_time: OffsetDateTime,
    pub walking_seconds: u32,
    pub transfer_count: u32,
    pub bicycle_seconds: u32,
    pub bike_share_uses: usize,
    pub evidence: Vec<NormalizedRealtimeValue>,
    pub timing_bases: Vec<TimingBasis>,
    pub timing_reliability: TimingReliability,
}

impl PathMetrics {
    pub fn from_movements(movements: &[JourneyMovement], arrival_time: OffsetDateTime) -> Self {
        let mut result = Self {
            arrival_time,
            walking_seconds: 0,
            transfer_count: 0,
            bicycle_seconds: 0,
            bike_share_uses: 0,
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
        };
        let mut last_trip = None;
        let mut surface_since_transit = false;
        for movement in movements {
            match &movement.kind {
                JourneyMovementKind::Walking {
                    duration_seconds, ..
                } => {
                    result.walking_seconds =
                        result.walking_seconds.saturating_add(*duration_seconds);
                    if last_trip.is_some() {
                        surface_since_transit = true;
                    }
                }
                JourneyMovementKind::BikeShare {
                    movement: bike_movement,
                } => {
                    result.walking_seconds = result
                        .walking_seconds
                        .saturating_add(bike_movement.walk_to_pickup.duration_seconds)
                        .saturating_add(bike_movement.walk_from_dropoff.duration_seconds);
                    result.bicycle_seconds = result
                        .bicycle_seconds
                        .saturating_add(bike_movement.bicycle_duration_seconds);
                    result.bike_share_uses += 1;
                    if last_trip.is_some() {
                        surface_since_transit = true;
                    }
                }
                JourneyMovementKind::Transit { trip, .. } => {
                    if last_trip
                        .as_ref()
                        .is_some_and(|id| surface_since_transit || id != &trip.id)
                    {
                        result.transfer_count += 1;
                    }
                    last_trip = Some(trip.id.clone());
                    surface_since_transit = false;
                    result.evidence.extend(trip.evidence.clone());
                    result.timing_bases.push(trip.timing_basis);
                    if trip.timing_reliability == TimingReliability::Fallback {
                        result.timing_reliability = TimingReliability::Fallback;
                    }
                }
            }
        }
        result
    }
    pub fn same_timing_quality(&self, other: &Self) -> bool {
        self.timing_reliability == other.timing_reliability
            && super::pareto::same_timing_bases(&self.timing_bases, &other.timing_bases)
            && self.evidence == other.evidence
    }
    pub fn dominates(&self, other: &Self) -> bool {
        (self.same_timing_quality(other)
            || (other.timing_reliability == TimingReliability::Reliable
                && other.timing_bases.is_empty()
                && other.evidence.is_empty()))
            && self.arrival_time <= other.arrival_time
            && self.walking_seconds <= other.walking_seconds
            && self.transfer_count <= other.transfer_count
            && self.bicycle_seconds <= other.bicycle_seconds
            && self.bike_share_uses <= other.bike_share_uses
            && (self.arrival_time < other.arrival_time
                || self.walking_seconds < other.walking_seconds
                || self.transfer_count < other.transfer_count
                || self.bicycle_seconds < other.bicycle_seconds
                || self.bike_share_uses < other.bike_share_uses)
    }
}

#[derive(Clone)]
pub(super) struct ActiveBikeRental {
    pub start_anchor: usize,
    pub started_at: OffsetDateTime,
    pub prefix_movements: Vec<JourneyMovement>,
    pub movement: BikeShareMovement,
}
impl ActiveBikeRental {
    pub fn reuse_available_at(&self) -> OffsetDateTime {
        self.started_at
            + Duration::seconds(i64::from(
                self.movement
                    .walk_to_pickup
                    .duration_seconds
                    .saturating_add(self.movement.bicycle_duration_seconds),
            ))
            + Duration::minutes(15)
    }
}
impl EvaluationState {
    pub fn bike_departure(&self, index: usize) -> (usize, OffsetDateTime) {
        self.active_bike_rental.as_ref().map_or_else(
            || {
                (
                    index,
                    self.bike_reuse_available_at
                        .map_or(self.ready_at, |time| time.max(self.ready_at)),
                )
            },
            |rental| (rental.start_anchor, rental.started_at),
        )
    }
    pub fn after_bike_share(
        &self,
        movement: BikeShareMovement,
        start_anchor: usize,
        to_anchor: usize,
        started_at: OffsetDateTime,
        arrival: OffsetDateTime,
        query_departure: OffsetDateTime,
    ) -> Option<Self> {
        let wait = self.active_bike_rental.as_ref().map_or_else(
            || u32::try_from((started_at - self.ready_at).whole_seconds()).unwrap_or(u32::MAX),
            |rental| rental.movement.rental_wait_seconds,
        );
        let movement = movement.with_rental_wait(wait)?;
        let prefix = self.active_bike_rental.as_ref().map_or_else(
            || self.movements.clone(),
            |rental| rental.prefix_movements.clone(),
        );
        let departure = prefix
            .last()
            .map_or(query_departure, |movement| movement.arrival_at);
        let mut movements = prefix.clone();
        movements.push(JourneyMovement {
            from_anchor: start_anchor,
            to_anchor,
            departure_at: departure,
            arrival_at: arrival,
            kind: JourneyMovementKind::BikeShare {
                movement: movement.clone(),
            },
        });
        Some(Self {
            ready_at: arrival,
            last_was_transit: false,
            active_bike_rental: Some(ActiveBikeRental {
                start_anchor,
                started_at,
                prefix_movements: prefix,
                movement,
            }),
            bike_reuse_available_at: self.bike_reuse_available_at,
            movements,
        })
    }
}
