//! Provider-neutral routing pipeline.

pub mod algorithm;
pub mod bike;
pub mod discover;
pub mod enrich;
pub mod graph;
pub mod reconstruct;
pub mod search;
pub mod transfers;
pub mod trip_based;

pub(crate) mod service;

pub use crate::domain::journey::{JourneyOption, PlanningError, PlanningOutcome};
pub use crate::domain::transit_timing::{
    DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
    ScheduleDepartureResolver, TimingBasis, TransitDeparture, TransitTimingError,
    TransitTimingProvider, TransitTimingQuery,
};
pub use crate::providers::realtime::RealtimeProvider;
pub use crate::street::StreetRouter;
pub use algorithm::{
    AccessPoint, AlgorithmQuery, EgressPoint, JourneyCandidate, JourneySegment, RoutingAlgorithm,
    RoutingError,
};
pub use graph::StreetMetric;
pub use service::{RoutingLimits, RoutingService};
pub use transfers::{
    BikeAvailabilityResolver, TransferError, TransferIndex, TransferLeg, TransferPath,
    TransferShortcut, TransferShortcutIndex,
};
pub use trip_based::{TripBasedIndex, TripBasedLimits, TripBasedRouter};

pub type RouteQuery = crate::api::PlanningRequest;
