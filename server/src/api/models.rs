//! HTTP-facing models. The canonical definitions remain in `api.rs` while
//! this namespace provides the modular API boundary for new callers.

pub use super::{
    DepartureSetting, JourneyMode, PlanningRequest, RequestValidationError, SelectedPlace,
};
