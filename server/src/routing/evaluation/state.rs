use super::*;

#[derive(Clone, Debug, PartialEq)]
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
        if let Some(return_at) = self.rental_reuse_available_at() {
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

    pub fn transit_ready_at(&self, transfer_seconds: u32) -> OffsetDateTime {
        if self.last_was_transit {
            self.ready_at + Duration::seconds(i64::from(transfer_seconds))
        } else {
            self.ready_at
        }
    }

    pub fn metrics(&self) -> PathMetrics {
        PathMetrics::from_movements(&self.movements, self.ready_at)
    }

    pub fn rental_bike_movement(&self) -> Option<&BikeShareMovement> {
        self.active_bike_rental.as_ref()?;
        match &self.movements.last()?.kind {
            JourneyMovementKind::BikeShare { movement } => Some(movement),
            JourneyMovementKind::Walking { .. } | JourneyMovementKind::Transit { .. } => None,
        }
    }

    pub fn rental_prefix(&self) -> Option<&[JourneyMovement]> {
        self.rental_bike_movement()?;
        self.movements.split_last().map(|(_, prefix)| prefix)
    }

    fn rental_reuse_available_at(&self) -> Option<OffsetDateTime> {
        let rental = self.active_bike_rental.as_ref()?;
        let movement = self.rental_bike_movement()?;
        Some(
            rental.started_at
                + Duration::seconds(i64::from(movement.seconds_to_return()))
                + BIKE_REUSE_COOLDOWN,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
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
            result.walking_seconds = result
                .walking_seconds
                .saturating_add(movement.kind.walking_seconds());
            result.bicycle_seconds = result
                .bicycle_seconds
                .saturating_add(movement.kind.bicycle_seconds());
            match &movement.kind {
                JourneyMovementKind::Walking { .. } => {
                    if last_trip.is_some() {
                        surface_since_transit = true;
                    }
                }
                JourneyMovementKind::BikeShare { .. } => {
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
            && same_timing_bases(&self.timing_bases, &other.timing_bases)
            && self.evidence == other.evidence
    }
    pub fn dominates(&self, other: &Self) -> bool {
        (self.same_timing_quality(other)
            || (other.timing_reliability == TimingReliability::Reliable
                && other.timing_bases.is_empty()
                && other.evidence.is_empty()))
            && self.costs_no_more_than(other)
            && (self.arrival_time < other.arrival_time
                || self.walking_seconds < other.walking_seconds
                || self.transfer_count < other.transfer_count
                || self.bicycle_seconds < other.bicycle_seconds
                || self.bike_share_uses < other.bike_share_uses)
    }
}

impl PathMetrics {
    pub fn no_worse_than(&self, other: &Self) -> bool {
        self.same_timing_quality(other) && self.costs_no_more_than(other)
    }

    fn costs_no_more_than(&self, other: &Self) -> bool {
        self.arrival_time <= other.arrival_time
            && self.walking_seconds <= other.walking_seconds
            && self.transfer_count <= other.transfer_count
            && self.bicycle_seconds <= other.bicycle_seconds
            && self.bike_share_uses <= other.bike_share_uses
    }
}

fn same_timing_bases(left: &[TimingBasis], right: &[TimingBasis]) -> bool {
    fn unique(bases: &[TimingBasis]) -> Vec<TimingBasis> {
        let mut normalized = Vec::new();
        for basis in bases {
            if normalized.last() != Some(basis) {
                normalized.push(*basis);
            }
        }
        normalized
    }
    unique(left) == unique(right)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ActiveBikeRental {
    pub start_anchor: usize,
    pub started_at: OffsetDateTime,
}

impl EvaluationState {
    pub fn same_capability(&self, other: &Self) -> bool {
        (!(self.movements.iter().any(needs_hydration)
            || other.movements.iter().any(needs_hydration))
            || self.movements == other.movements)
            && self.last_was_transit == other.last_was_transit
            && self.bike_reuse_available_at == other.bike_reuse_available_at
            && match (&self.active_bike_rental, &other.active_bike_rental) {
                (None, None) => true,
                (Some(left_rental), Some(right_rental)) => {
                    left_rental == right_rental
                        && self.rental_bike_movement() == other.rental_bike_movement()
                        && self.rental_prefix() == other.rental_prefix()
                }
                _ => false,
            }
    }

    pub fn bike_departure(&self, index: usize) -> ActiveBikeRental {
        self.active_bike_rental.unwrap_or_else(|| ActiveBikeRental {
            start_anchor: index,
            started_at: self
                .bike_reuse_available_at
                .map_or(self.ready_at, |time| time.max(self.ready_at)),
        })
    }

    pub fn after_walk(
        &self,
        query: &PlanningQuery,
        walking: JourneyMovementKind,
        duration_seconds: u32,
        index: usize,
    ) -> Option<Self> {
        let arrival = self.ready_at + Duration::seconds(i64::from(duration_seconds));
        query.admits(arrival).then(|| {
            self.after_movement(JourneyMovement {
                from_anchor: index,
                to_anchor: index + 1,
                departure_at: self.ready_at,
                arrival_at: arrival,
                kind: walking,
            })
        })
    }

    pub fn after_bike_shares(
        &self,
        query: &PlanningQuery,
        movements: Vec<BikeShareMovement>,
        departure: ActiveBikeRental,
        index: usize,
    ) -> Vec<Self> {
        movements
            .into_iter()
            .filter_map(|movement| {
                let arrival =
                    departure.started_at + Duration::seconds(i64::from(movement.total_seconds));
                if !query.admits(arrival) {
                    return None;
                }
                self.after_bike_share(
                    movement,
                    departure,
                    index + 1,
                    arrival,
                    query.departure_time,
                )
            })
            .collect()
    }

    pub fn after_bike_share(
        &self,
        movement: BikeShareMovement,
        departure: ActiveBikeRental,
        to_anchor: usize,
        arrival: OffsetDateTime,
        query_departure: OffsetDateTime,
    ) -> Option<Self> {
        let wait = self.rental_bike_movement().map_or_else(
            || {
                u32::try_from((departure.started_at - self.ready_at).whole_seconds())
                    .unwrap_or(u32::MAX)
            },
            |rental_movement| rental_movement.rental_wait_seconds,
        );
        let movement = movement.with_rental_wait(wait)?;
        let prefix = self
            .rental_prefix()
            .map_or_else(|| self.movements.clone(), <[JourneyMovement]>::to_vec);
        let departure_at = prefix
            .last()
            .map_or(query_departure, |movement| movement.arrival_at);
        let mut movements = prefix;
        movements.push(JourneyMovement {
            from_anchor: departure.start_anchor,
            to_anchor,
            departure_at,
            arrival_at: arrival,
            kind: JourneyMovementKind::BikeShare { movement },
        });
        Some(Self {
            ready_at: arrival,
            last_was_transit: false,
            active_bike_rental: Some(departure),
            bike_reuse_available_at: self.bike_reuse_available_at,
            movements,
        })
    }
}

pub(super) fn needs_hydration(movement: &JourneyMovement) -> bool {
    !matches!(movement.kind, JourneyMovementKind::Transit { .. })
}
