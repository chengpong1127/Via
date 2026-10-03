use time::{Duration, OffsetDateTime};
use tokio::time::Instant;

use crate::{
    api::PlanningRequest,
    domain::{journey::PlanningError, normalize_to_taipei, transit::TransitMode},
    street::{RoutingConstraints, RoutingTime, RoutingTimeKind},
};

use crate::domain::{ids::BoardingPointId, transit_timing::DepartureError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanningDeadline {
    expires_at: Instant,
}

impl PlanningDeadline {
    pub fn after(timeout: std::time::Duration) -> Self {
        Self {
            expires_at: Instant::now() + timeout,
        }
    }

    pub fn is_elapsed(self) -> bool {
        Instant::now() >= self.expires_at
    }

    pub(crate) fn instant(self) -> Instant {
        self.expires_at
    }

    #[cfg(test)]
    pub(crate) fn elapsed_for_test() -> Self {
        Self {
            expires_at: Instant::now(),
        }
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
    pub max_candidates: usize,
    pub max_phase2_expansions: usize,
    pub max_departures_per_query: usize,
    pub max_journey_duration_seconds: u32,
}

impl Default for PlannerLimits {
    fn default() -> Self {
        Self {
            max_transfers: 4,
            max_static_corridors: 100,
            max_phase1_expansions: 20_000,
            max_candidates: 16,
            max_phase2_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
        }
    }
}

impl PlannerLimits {
    pub(crate) fn normalized(self) -> Self {
        Self {
            max_transfers: self.max_transfers,
            max_static_corridors: self.max_static_corridors.max(1),
            max_phase1_expansions: self.max_phase1_expansions.max(1),
            max_candidates: self.max_candidates.max(1),
            max_phase2_expansions: self.max_phase2_expansions.max(1),
            max_departures_per_query: self.max_departures_per_query.max(1),
            max_journey_duration_seconds: self.max_journey_duration_seconds.max(1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct JourneyPlanningLimits {
    pub planner: PlannerLimits,
    pub request_timeout: std::time::Duration,
    pub max_concurrent_requests: usize,
    pub max_transit_access_stops: usize,
    pub max_transit_egress_stops: usize,
    pub max_walking_radius_metres: u32,
    pub max_walking_seconds: u32,
}

impl Default for JourneyPlanningLimits {
    fn default() -> Self {
        Self {
            planner: PlannerLimits::default(),
            request_timeout: std::time::Duration::from_secs(5),
            max_concurrent_requests: 32,
            max_transit_access_stops: 24,
            max_transit_egress_stops: 24,
            max_walking_radius_metres: 2_000,
            max_walking_seconds: 30 * 60,
        }
    }
}

impl JourneyPlanningLimits {
    pub(crate) fn normalized(self) -> Self {
        Self {
            planner: self.planner.normalized(),
            request_timeout: self.request_timeout,
            max_concurrent_requests: self.max_concurrent_requests.max(1),
            max_transit_access_stops: self.max_transit_access_stops.max(1),
            max_transit_egress_stops: self.max_transit_egress_stops.max(1),
            max_walking_radius_metres: self.max_walking_radius_metres.max(1),
            max_walking_seconds: self.max_walking_seconds.max(1),
        }
    }
}

#[derive(Clone)]
pub(crate) struct JourneyPlanningContext<'a> {
    pub(crate) request: &'a PlanningRequest,
    pub(crate) search_start: OffsetDateTime,
    pub(crate) latest_arrival: Option<OffsetDateTime>,
    pub(crate) deadline: PlanningDeadline,
}

impl<'a> JourneyPlanningContext<'a> {
    pub(crate) fn new(
        request: &'a PlanningRequest,
        request_started_at: OffsetDateTime,
        max_journey_duration_seconds: u32,
        deadline: PlanningDeadline,
    ) -> Result<Self, PlanningError> {
        let effective_search_time = request
            .departure_setting
            .effective_search_time(request_started_at);
        let (search_start, latest_arrival) = match request.departure_setting {
            crate::api::DepartureSetting::ArriveBy { .. } => (
                effective_search_time - Duration::seconds(i64::from(max_journey_duration_seconds)),
                Some(effective_search_time),
            ),
            crate::api::DepartureSetting::DepartNow
            | crate::api::DepartureSetting::DepartAt { .. } => (effective_search_time, None),
        };
        Ok(Self {
            request,
            search_start,
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
pub struct TransitQuery {
    pub origin: Option<crate::street::Coordinate>,
    pub destination: Option<crate::street::Coordinate>,
    pub origin_access: Vec<AccessPoint>,
    pub destination_egress: Vec<EgressPoint>,
    pub allowed_transit_modes: Vec<TransitMode>,

    pub allow_bike_share: bool,
    pub departure_time: OffsetDateTime,
    pub latest_arrival: Option<OffsetDateTime>,
    pub limits: PlannerLimits,
    pub deadline: Option<PlanningDeadline>,
}

impl TransitQuery {
    pub fn new(
        origin_access: Vec<AccessPoint>,
        destination_egress: Vec<EgressPoint>,
        departure_time: OffsetDateTime,
    ) -> Self {
        Self {
            origin: None,
            destination: None,
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
            limits: PlannerLimits::default(),
            deadline: None,
        }
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

    pub fn with_endpoints(
        mut self,
        origin: crate::street::Coordinate,
        destination: crate::street::Coordinate,
    ) -> Self {
        self.origin = Some(origin);
        self.destination = Some(destination);
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

    pub fn with_deadline(mut self, deadline: PlanningDeadline) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoutingError {
    #[error("routing query has no usable access or egress")]
    InvalidQuery,
    #[error("static corridor search exceeded its safety expansion bound")]
    StaticSearchLimit,
    #[error("fixed-corridor evaluation exceeded its safety expansion bound")]
    Phase2EvaluationLimit,
    #[error("routing request deadline elapsed")]
    DeadlineExceeded,
    #[error("timing snapshot could not be loaded: {0}")]
    Timing(#[from] DepartureError),
}
