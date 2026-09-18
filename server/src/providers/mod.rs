//! Concrete external-system adapters.
//!
//! Routing imports provider-neutral traits from `domain`/`routing`; these
//! modules are the only stable public entry points for concrete adapters.

pub mod service_conditions;
pub mod timing;

pub mod valhalla;

pub use service_conditions::{
    CachedServiceConditionProvider, NullServiceConditionProvider, ServiceConditionError,
    ServiceConditionMetrics, ServiceConditionProvider, TdxServiceConditionProvider,
};
pub use timing::{
    BusDepartureSource, DepartureDispatcher, MetroDepartureSource, ThsrDepartureSource,
    TraDepartureSource, TransitDepartureSource,
};
pub use valhalla::{
    NoopOperationalEventObserver, OperationOutcome, OperationalEvent, OperationalEventObserver,
    RoutingOperation, ValhallaConfig, ValhallaProvider,
};
