//! Concrete external-system adapters.
//!
//! Routing imports provider-neutral traits from `domain`/`routing`; these
//! modules are the only stable public entry points for concrete adapters.

pub mod realtime;
pub mod tdx;
pub mod timing;

pub mod valhalla;

pub use tdx::{TdxRealtimeConfig, TdxRealtimeProvider};
pub use timing::{
    CompositeDepartureResolver, RealtimeBikeAvailabilityResolver, RealtimeDepartureResolver,
};
pub use valhalla::{
    NoopOperationalEventObserver, OperationOutcome, OperationalEvent, OperationalEventObserver,
    RoutingOperation, ValhallaConfig, ValhallaProvider,
};
