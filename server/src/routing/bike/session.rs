use super::*;

pub(crate) struct BikeShareSession {
    planner: BikeShareMovementPlanner,
    street: Arc<dyn StreetRouter>,
    snapshot: Arc<BikeAvailabilitySnapshot>,
    now: OffsetDateTime,
    movements: RequestCache<
        (CorridorAnchorKey, CorridorAnchorKey),
        Result<Vec<BikeShareMovement>, StreetRoutingError>,
    >,
}

impl BikeShareSession {
    pub(crate) fn new(
        planner: BikeShareMovementPlanner,
        snapshot: Arc<BikeAvailabilitySnapshot>,
        now: OffsetDateTime,
        street: Arc<dyn StreetRouter>,
    ) -> Self {
        Self {
            planner,
            street,
            snapshot,
            now,
            movements: RequestCache::default(),
        }
    }

    pub(crate) fn requests(&self) -> usize {
        self.movements.entries()
    }

    pub(crate) fn hits(&self) -> usize {
        self.movements.hits()
    }
}

#[async_trait::async_trait]
impl BikeMovementSource for BikeShareSession {
    async fn movements(
        &self,
        from: &CorridorAnchor,
        to: &CorridorAnchor,
        from_anchor: usize,
        to_anchor: usize,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        let movements = self
            .movements
            .get_or_init((from.cache_key(), to.cache_key()), || async {
                match self
                    .planner
                    .movement_candidate(from, to, &self.snapshot, self.now)
                {
                    Some(candidate) => self.planner.realize(self.street.as_ref(), &candidate).await,
                    None => Ok(Vec::new()),
                }
            })
            .await?;
        Ok(movements
            .into_iter()
            .map(|mut movement| {
                movement.from_anchor = from_anchor;
                movement.to_anchor = to_anchor;
                movement
            })
            .collect())
    }
}
