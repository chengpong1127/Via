//! Provider-neutral routing pipeline.

pub(crate) mod access;
pub mod algorithm;
pub mod bike;
pub mod reconstruct;
pub mod transfers;
pub mod trip_based;

pub(crate) mod service;
pub(crate) mod status;

pub use crate::domain::journey::{JourneyOption, PlanningError, PlanningOutcome};
pub use crate::domain::transit_timing::{
    DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
    RealtimeFirstDepartureResolver, ScheduleDepartureResolver, TimingBasis,
};
pub use crate::providers::realtime::RealtimeProvider;
pub use crate::street::StreetRouter;
pub use algorithm::{
    AccessPoint, AlgorithmQuery, EgressPoint, JourneyCandidate, JourneySegment, RoutingAlgorithm,
    RoutingError,
};
pub use bike::{
    BikeAvailabilityError, BikeAvailabilityResolver, EndpointLeg, EndpointPath,
    bike_availability_is_usable,
};
pub use service::{RoutingLimits, RoutingService};
pub use transfers::{
    TransferIndex, TransferLeg, TransferPath, TransferShortcut, TransferShortcutIndex,
};
pub use trip_based::{TripBasedIndex, TripBasedLimits, TripBasedRouter};

pub type RouteQuery = crate::api::PlanningRequest;
