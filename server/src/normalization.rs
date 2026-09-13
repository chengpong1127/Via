//! Compatibility exports for the pre-domain normalization module path.
//!
//! Canonical Via identities and normalized transit values live under
//! [`crate::domain`]. Provider adapters may continue importing this facade
//! during the migration.

pub use crate::domain::ids::{
    AliasError, AliasKey, AliasRegistry, BikeShareStationId, BoardingPointId, EntityId, OperatorId,
    Provider, ScheduledRunId, ServicePatternId, StopPlaceId, TransitServiceId, VehicleId,
};
pub use crate::domain::realtime::{
    ActionState, ArrivalEstimate, ArrivalState, BikeAvailability, LocalizedSourceText,
    ServiceCondition, ServiceConditionState, ServiceDayPolicy,
};
