mod source;

mod bus;
mod bus_realtime;
mod metro;
mod metro_realtime;
mod thsr;
mod tra;

pub use crate::providers::tdx_bindings::{
    BusPatternBinding, MetroPatternBinding, TdxProviderBindings, TraTripBinding,
};
pub use bus::BusDepartureSource;
pub use bus_realtime::normalize_n1;
pub use metro::MetroDepartureSource;
pub use source::{DepartureDispatcher, TransitDepartureSource};
pub use thsr::ThsrDepartureSource;
pub use tra::TraDepartureSource;
