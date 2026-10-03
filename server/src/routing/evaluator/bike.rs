//! BikeShare rental continuation and reuse timing within a fixed corridor.
use super::*;

#[derive(Clone)]
pub(super) struct ActiveBikeRental {
    pub start_anchor: usize,
    pub started_at: OffsetDateTime,
    pub prefix_movements: Vec<JourneyMovement>,
    pub shortcut: BikeShortcut,
}
impl ActiveBikeRental {
    pub fn reuse_available_at(&self) -> OffsetDateTime {
        self.started_at
            + Duration::seconds(i64::from(
                self.shortcut
                    .walk_to_pickup
                    .duration_seconds
                    .saturating_add(self.shortcut.bicycle_duration_seconds),
            ))
            + Duration::minutes(15)
    }
}
impl EvaluationState {
    pub fn bike_departure(&self, index: usize) -> (usize, OffsetDateTime) {
        self.active_bike_run.as_ref().map_or_else(
            || {
                (
                    index,
                    self.bike_reuse_available_at
                        .map_or(self.ready_at, |time| time.max(self.ready_at)),
                )
            },
            |run| (run.start_anchor, run.started_at),
        )
    }
    pub fn after_bike_share(
        &self,
        shortcut: BikeShortcut,
        start_anchor: usize,
        to_anchor: usize,
        started_at: OffsetDateTime,
        arrival: OffsetDateTime,
        query_departure: OffsetDateTime,
    ) -> Option<Self> {
        let wait = self.active_bike_run.as_ref().map_or_else(
            || u32::try_from((started_at - self.ready_at).whole_seconds()).unwrap_or(u32::MAX),
            |run| run.shortcut.rental_wait_seconds,
        );
        let shortcut = shortcut.with_rental_wait(wait)?;
        let prefix = self.active_bike_run.as_ref().map_or_else(
            || self.movements.clone(),
            |run| run.prefix_movements.clone(),
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
                shortcut: shortcut.clone(),
            },
            street_path: None,
        });
        Some(Self {
            ready_at: arrival,
            last_was_transit: false,
            active_bike_run: Some(ActiveBikeRental {
                start_anchor,
                started_at,
                prefix_movements: prefix,
                shortcut,
            }),
            bike_reuse_available_at: self.bike_reuse_available_at,
            movements,
        })
    }
}
