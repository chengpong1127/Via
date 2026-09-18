//! Provider-neutral routing pipeline.

pub(crate) mod access;
pub mod algorithm;
pub mod reconstruct;
pub mod transfers;
pub mod trip_based;

pub(crate) mod service;
pub(crate) mod status;

pub use crate::domain::journey::{JourneyOption, PlanningError, PlanningOutcome};
pub use crate::domain::transit_timing::{
    DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
    ScheduleDepartureResolver, TimingBasis, TimingReliability,
};
pub use crate::providers::service_conditions::ServiceConditionProvider;
pub use crate::street::StreetRouter;
pub use algorithm::{
    AccessPoint, AlgorithmQuery, EgressPoint, JourneyCandidate, JourneySegment, RoutingAlgorithm,
    RoutingError,
};
pub use service::{RoutingLimits, RoutingService};
pub use transfers::{
    ResolvedTransfer, TransferIndex, TransferNeed, TransferResolver, TransferShortcut,
    TransferShortcutIndex, TransferTarget,
};
pub use trip_based::{TripBasedIndex, TripBasedLimits, TripBasedRouter};

pub type RouteQuery = crate::api::PlanningRequest;
