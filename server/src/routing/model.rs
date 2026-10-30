use time::{Duration, OffsetDateTime};
use tokio::time::Instant;

use crate::{
    domain::planning::PlanningInput,
    domain::{journey::PlanningError, normalize_to_taipei, transit::TransitMode},
    street::{
        ProviderFailureClass, RoutingConstraints, RoutingTime, RoutingTimeKind, StreetRoutingError,
    },
};

use crate::domain::{ids::BoardingPointId, transit_timing::DepartureError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanningDeadline {
    Disabled,
    At(Instant),
}

impl PlanningDeadline {
    pub fn after(timeout: std::time::Duration) -> Self {
        Self::At(Instant::now() + timeout)
    }

    pub fn disabled() -> Self {
        Self::Disabled
    }

    pub fn is_elapsed(self) -> bool {
        matches!(self, Self::At(expires_at) if Instant::now() >= expires_at)
    }

    pub(crate) async fn run<F: std::future::Future>(
        self,
        future: F,
    ) -> Result<F::Output, RoutingError> {
        match self {
            Self::Disabled => Ok(future.await),
            Self::At(expires_at) => tokio::time::timeout_at(expires_at, future)
                .await
                .map_err(|_| RoutingError::DeadlineExceeded),
        }
    }

    #[cfg(test)]
    pub(crate) fn elapsed_for_test() -> Self {
        Self::At(Instant::now())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    pub stop_id: BoardingPointId,
    pub departure_at: OffsetDateTime,
    pub available_at: OffsetDateTime,
    pub walking_seconds: u32,
    pub distance_metres: u32,
}

impl AccessPoint {
    pub fn new(
        stop_id: BoardingPointId,
        departure_at: OffsetDateTime,
        available_at: OffsetDateTime,
        walking_seconds: u32,
        distance_metres: u32,
    ) -> Self {
        Self {
            stop_id,
            departure_at,
            available_at,
            walking_seconds,
            distance_metres,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressPoint {
    pub stop_id: BoardingPointId,
    pub walking_seconds: u32,
    pub distance_metres: u32,
}

impl EgressPoint {
    pub fn new(stop_id: BoardingPointId, walking_seconds: u32, distance_metres: u32) -> Self {
        Self {
            stop_id,
            walking_seconds,
            distance_metres,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannerLimits {
    pub max_transfers: usize,
    pub max_static_corridors: usize,
    pub max_phase1_expansions: usize,
    pub max_concurrent_phase2_states: usize,
    pub max_concurrent_phase2_corridors: usize,
    pub max_candidates: usize,
    pub max_departures_per_query: usize,
    pub max_journey_duration_seconds: u32,
}

impl PlannerLimits {
    pub(crate) fn normalized(self) -> Self {
        Self {
            max_transfers: self.max_transfers,
            max_static_corridors: self.max_static_corridors.max(1),
            max_phase1_expansions: self.max_phase1_expansions.max(1),
            max_concurrent_phase2_states: self.max_concurrent_phase2_states.clamp(1, 32),
            max_concurrent_phase2_corridors: self.max_concurrent_phase2_corridors.clamp(1, 8),
            max_candidates: self.max_candidates.max(1),
            max_departures_per_query: self.max_departures_per_query.max(1),
            max_journey_duration_seconds: self.max_journey_duration_seconds.max(1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct JourneyPlanningLimits {
    pub bike: super::BikeMovementPolicy,
    pub ranking: crate::domain::ranking::RankingPolicy,
    pub planner: PlannerLimits,
    pub request_timeout: std::time::Duration,
    pub max_concurrent_requests: usize,
    pub max_concurrent_reconstruction_candidates: usize,
    pub max_transit_access_stops: usize,
    pub max_transit_egress_stops: usize,
    pub max_walking_radius_metres: u32,
    pub max_walking_seconds: u32,
}

impl JourneyPlanningLimits {
    pub(crate) fn normalized(self) -> Self {
        Self {
            bike: self.bike,
            ranking: self.ranking,
            planner: self.planner.normalized(),
            request_timeout: self.request_timeout,
            max_concurrent_requests: self.max_concurrent_requests.max(1),
            max_concurrent_reconstruction_candidates: self
                .max_concurrent_reconstruction_candidates
                .clamp(1, 16),
            max_transit_access_stops: self.max_transit_access_stops.max(1),
            max_transit_egress_stops: self.max_transit_egress_stops.max(1),
            max_walking_radius_metres: self.max_walking_radius_metres.max(1),
            max_walking_seconds: self.max_walking_seconds.max(1),
        }
    }
}

#[derive(Clone)]
pub(crate) struct JourneyPlanningContext<'a> {
    pub(crate) request: &'a PlanningInput,
    pub(crate) search_start: OffsetDateTime,
    pub(crate) max_journey_duration_seconds: u32,
    pub(crate) latest_arrival: Option<OffsetDateTime>,
    pub(crate) deadline: PlanningDeadline,
}

impl<'a> JourneyPlanningContext<'a> {
    pub(crate) fn new(
        request: &'a PlanningInput,
        request_started_at: OffsetDateTime,
        max_journey_duration_seconds: u32,
        deadline: PlanningDeadline,
    ) -> Result<Self, PlanningError> {
        let (search_start, latest_arrival) = request
            .departure
            .search_window(request_started_at, max_journey_duration_seconds);
        Ok(Self {
            request,
            search_start,
            max_journey_duration_seconds,
            latest_arrival,
            deadline,
        })
    }

    pub(crate) fn ensure_before_deadline(&self) -> Result<(), PlanningError> {
        if self.deadline.is_elapsed() {
            Err(PlanningError::TemporarilyUnavailable)
        } else {
            Ok(())
        }
    }

    fn local_time(&self, instant: OffsetDateTime) -> time::PrimitiveDateTime {
        let instant = normalize_to_taipei(instant);
        instant.date().with_time(instant.time())
    }

    pub(crate) fn constraints(
        &self,
        instant: OffsetDateTime,
        kind: RoutingTimeKind,
    ) -> RoutingConstraints {
        RoutingConstraints {
            time: Some(RoutingTime {
                kind,
                local_date_time: self.local_time(instant),
            }),
            max_distance: None,
        }
    }

    pub(crate) fn access_constraints(&self) -> RoutingConstraints {
        self.constraints(self.search_start, RoutingTimeKind::DepartAt)
    }

    pub(crate) fn direct_constraints(&self) -> RoutingConstraints {
        match self.latest_arrival {
            Some(latest_arrival) => self.constraints(latest_arrival, RoutingTimeKind::ArriveBy),
            None => self.constraints(self.search_start, RoutingTimeKind::DepartAt),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanningQuery {
    pub origin: crate::street::Coordinate,
    pub destination: crate::street::Coordinate,
    pub origin_access: Vec<AccessPoint>,
    pub destination_egress: Vec<EgressPoint>,
    pub allowed_transit_modes: Vec<TransitMode>,

    pub allow_bike_share: bool,
    pub departure_time: OffsetDateTime,
    pub latest_arrival: Option<OffsetDateTime>,
    pub limits: PlannerLimits,
    pub deadline: PlanningDeadline,
}

impl PlanningQuery {
    pub fn new(
        origin: crate::street::Coordinate,
        destination: crate::street::Coordinate,
        deadline: PlanningDeadline,
        origin_access: Vec<AccessPoint>,
        destination_egress: Vec<EgressPoint>,
        departure_time: OffsetDateTime,
        limits: PlannerLimits,
    ) -> Self {
        Self {
            origin,
            destination,
            origin_access,
            destination_egress,
            allowed_transit_modes: vec![
                TransitMode::Bus,
                TransitMode::Metro,
                TransitMode::Rail,
                TransitMode::HighSpeedRail,
            ],
            allow_bike_share: false,
            departure_time,
            latest_arrival: None,
            limits,
            deadline,
        }
    }

    pub(crate) fn validate_access_egress(&self) -> Result<(), RoutingError> {
        let mut access = std::collections::HashMap::new();
        for point in &self.origin_access {
            if point.departure_at != self.departure_time
                || point.available_at - point.departure_at
                    != Duration::seconds(i64::from(point.walking_seconds))
                || access
                    .insert(&point.stop_id, point)
                    .is_some_and(|known| known != point)
            {
                return Err(RoutingError::InvalidQuery);
            }
        }
        let mut egress = std::collections::HashMap::new();
        for point in &self.destination_egress {
            if egress
                .insert(&point.stop_id, point)
                .is_some_and(|known| known != point)
            {
                return Err(RoutingError::InvalidQuery);
            }
        }
        Ok(())
    }

    pub fn with_allowed_transit_modes(
        mut self,
        modes: impl IntoIterator<Item = TransitMode>,
    ) -> Self {
        self.allowed_transit_modes = modes.into_iter().collect();
        self.allowed_transit_modes.sort();
        self.allowed_transit_modes.dedup();
        self
    }

    pub fn with_bike_share(mut self, allow: bool) -> Self {
        self.allow_bike_share = allow;
        self
    }

    pub fn with_limits(mut self, limits: PlannerLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_max_static_corridors(mut self, max_static_corridors: usize) -> Self {
        self.limits.max_static_corridors = max_static_corridors.max(1);
        self
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoutingError {
    #[error("routing query has invalid access or egress")]
    InvalidQuery,
    #[error("static corridor search exceeded its safety expansion bound")]
    StaticSearchLimit,
    #[error("routing request deadline elapsed")]
    DeadlineExceeded,
    #[error("routing invariant failed: {0}")]
    Internal(String),
    #[error("timing snapshot could not be loaded: {0}")]
    Timing(#[from] DepartureError),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FailureSummary {
    pub(crate) transient: bool,
    pub(crate) internal: bool,
}

impl FailureSummary {
    pub(crate) fn record_provider(&mut self, error: &StreetRoutingError) {
        match error.failure_class() {
            ProviderFailureClass::Unreachable => {}
            ProviderFailureClass::Transient => self.transient = true,
            ProviderFailureClass::Internal => self.internal = true,
        }
    }

    pub(crate) fn record_planning(&mut self, error: &PlanningError) {
        self.transient |= *error == PlanningError::TemporarilyUnavailable;
        self.internal |= *error == PlanningError::Internal;
    }

    pub(crate) fn record_routing(&mut self, error: &RoutingError) {
        match error {
            RoutingError::Timing(timing) => self.transient |= timing.is_transient(),
            RoutingError::StaticSearchLimit | RoutingError::Internal(_) => self.internal = true,
            RoutingError::InvalidQuery | RoutingError::DeadlineExceeded => {}
        }
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.transient |= other.transient;
        self.internal |= other.internal;
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::{PlanningDeadline, RoutingError};
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn disabled_deadline_never_expires_and_awaits_the_future() {
        let deadline = PlanningDeadline::disabled();
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(!deadline.is_elapsed());
        let result = deadline
            .run(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                42
            })
            .await;
        assert_eq!(result, Ok(42));
        assert!(!deadline.is_elapsed());
    }

    #[tokio::test(start_paused = true)]
    async fn enabled_deadline_cancels_pending_work_at_its_expiration() {
        let deadline = PlanningDeadline::after(Duration::from_secs(5));
        assert!(!deadline.is_elapsed());
        assert_eq!(
            deadline.run(std::future::pending::<()>()).await,
            Err(RoutingError::DeadlineExceeded)
        );
        assert!(deadline.is_elapsed());
    }

    #[tokio::test(start_paused = true)]
    async fn enabled_deadline_allows_work_completed_before_expiration() {
        let deadline = PlanningDeadline::after(Duration::from_secs(5));
        assert_eq!(deadline.run(async { 42 }).await, Ok(42));
        assert!(!deadline.is_elapsed());
    }

    #[test]
    fn explicit_expired_deadline_is_elapsed() {
        assert!(PlanningDeadline::At(tokio::time::Instant::now()).is_elapsed());
    }
}
