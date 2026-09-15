//! Strongly typed domain IDs.

use serde::{Deserialize, Serialize};
macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_id!(OperatorId);
opaque_id!(StopPlaceId);
opaque_id!(BoardingPointId);
opaque_id!(BikeShareStationId);
opaque_id!(TransitServiceId);
opaque_id!(ServicePatternId);
opaque_id!(ScheduledRunId);
// A query-scoped identity for a concrete trip instance. Scheduled trips may
// use a stable ScheduledRunId; realtime-generated values are request-scoped.
opaque_id!(EffectiveTripId);
opaque_id!(VehicleId);
