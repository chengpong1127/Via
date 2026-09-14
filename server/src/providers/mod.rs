//! Concrete external-system adapters.
//!
//! Routing imports provider-neutral traits from `domain`/`routing`; these
//! modules are the only stable public entry points for concrete adapters.

pub mod realtime;
pub mod tdx;

pub mod valhalla;

pub use crate::street::GraphRevision;
pub use tdx::{TdxRealtimeConfig, TdxRealtimeProvider};
pub use valhalla::{
    NoopOperationalEventObserver, OperationOutcome, OperationalEvent, OperationalEventObserver,
    RoutingOperation, ValhallaConfig, ValhallaProvider,
};
