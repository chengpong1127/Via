//! Concrete external-system adapters.
//!
//! Routing imports provider-neutral traits from `domain`/`routing`; these
//! modules are the only stable public entry points for concrete adapters.

pub mod realtime;
pub mod tdx;
pub mod timing;

pub mod valhalla;

pub use crate::config::{TdxClient, TdxCredentials, TdxCredentialsError};
pub use crate::domain::transit_timing::RealtimeFirstDepartureResolver;
pub use tdx::{TdxRealtimeConfig, TdxRealtimeProvider};
pub use timing::{CompositeDepartureResolver, MetroDepartureResolver, RealtimeDepartureResolver};
pub use valhalla::{
    NoopOperationalEventObserver, OperationOutcome, OperationalEvent, OperationalEventObserver,
    RoutingOperation, ValhallaConfig, ValhallaProvider,
};
