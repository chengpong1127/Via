use super::*;

struct BikeWork<'a> {
    stats: BikePlanningStats,
    total: &'a AtomicUsize,
}

impl Drop for BikeWork<'_> {
    fn drop(&mut self) {
        self.total
            .fetch_add(self.stats.matrix_requests, Ordering::Relaxed);
    }
}

pub(crate) struct BikeShareSession {
    realizer: BikeMovementRealizer,
    snapshot: Arc<BikeAvailabilitySnapshot>,
    now: OffsetDateTime,
    cache: RequestCache<
        (CorridorAnchorKey, CorridorAnchorKey),
        Result<Vec<BikeShareMovement>, StreetRoutingError>,
    >,
    matrix_requests: AtomicUsize,
    candidates:
        Mutex<HashMap<(CorridorAnchorKey, CorridorAnchorKey), Option<BikeMovementCandidate>>>,
    #[cfg(test)]
    pub(super) candidate_discoveries: AtomicUsize,
    #[cfg(test)]
    test_planner: Option<Box<dyn TestBikePlanner>>,
}

impl BikeShareSession {
    pub(crate) fn new(
        planner: BikeShareMovementPlanner,
        snapshot: Arc<BikeAvailabilitySnapshot>,
        now: OffsetDateTime,
        street_router: Arc<dyn StreetRouter>,
    ) -> Self {
        Self {
            realizer: BikeMovementRealizer::new(planner, street_router),
            snapshot,
            now,
            cache: RequestCache::default(),
            matrix_requests: AtomicUsize::new(0),
            candidates: Mutex::new(HashMap::new()),
            #[cfg(test)]
            candidate_discoveries: AtomicUsize::new(0),
            #[cfg(test)]
            test_planner: None,
        }
    }

    pub fn realization_stats(&self) -> BikeSessionStats {
        #[cfg(test)]
        if let Some(planner) = &self.test_planner {
            return planner.realization_stats();
        }
        let cache = self.cache.stats();
        BikeSessionStats {
            exact_requests: cache.completed_entries,
            cache_hits: cache.ready_hits + cache.inflight_hits,
            matrix_requests: self.matrix_requests.load(Ordering::Relaxed),
        }
    }
    pub fn candidate(&self, request: &BikeMovementRequest<'_>) -> Option<BikeMovementCandidate> {
        #[cfg(test)]
        if let Some(planner) = &self.test_planner {
            return planner
                .has_candidate(request)
                .then(|| BikeMovementCandidate {
                    pickups: Vec::new(),
                    dropoffs: Vec::new(),
                    pickup_endpoint: None,
                    dropoff_endpoint: None,
                });
        }
        let key = (request.from.cache_key(), request.to.cache_key());
        self.candidates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_insert_with(|| {
                #[cfg(test)]
                {
                    self.candidate_discoveries.fetch_add(1, Ordering::Relaxed);
                }
                self.realizer.planner().movement_candidate(
                    request.from,
                    request.to,
                    &self.snapshot,
                    self.now,
                )
            })
            .clone()
    }

    #[cfg(test)]
    pub fn for_test(planner: Box<dyn TestBikePlanner>) -> Self {
        let mut session = Self::new(
            BikeShareMovementPlanner::new(
                Arc::new(BikeShareNetwork::new(Vec::new())),
                crate::config::ViaConfig::default().bike_policy(),
            ),
            Arc::new(BikeAvailabilitySnapshot::new(
                OffsetDateTime::now_utc(),
                HashMap::new(),
            )),
            OffsetDateTime::now_utc(),
            Arc::new(crate::street::FakeStreetRouter::builder().build()),
        );
        session.test_planner = Some(planner);
        session
    }

    pub async fn realize(
        &self,
        request: &BikeMovementRequest<'_>,
        candidate: &BikeMovementCandidate,
    ) -> Result<Vec<BikeShareMovement>, StreetRoutingError> {
        #[cfg(test)]
        if let Some(planner) = &self.test_planner {
            return planner.realize(request).await;
        }
        let key = (request.from.cache_key(), request.to.cache_key());
        self.cache
            .get_or_init(key, || async {
                let mut work = BikeWork {
                    stats: BikePlanningStats::default(),
                    total: &self.matrix_requests,
                };
                self.realizer
                    .realize_with_stats(candidate, 0, 1, &mut work.stats)
                    .await
            })
            .await
            .map(|bike_movements| {
                bike_movements
                    .into_iter()
                    .map(|mut bike_movement| {
                        bike_movement.from_anchor = request.from_anchor;
                        bike_movement.to_anchor = request.to_anchor;
                        bike_movement
                    })
                    .collect()
            })
    }
}
