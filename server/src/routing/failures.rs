use crate::{
    domain::journey::PlanningError,
    street::{ProviderFailureClass, StreetRoutingError},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FailureSummary {
    pub(crate) transient: bool,
    pub(crate) internal: bool,
}

impl FailureSummary {
    pub(crate) fn record_provider(&mut self, error: &StreetRoutingError) {
        match error.failure_class() {
            ProviderFailureClass::Unreachable => {}
            ProviderFailureClass::Transient => self.transient = true,
            ProviderFailureClass::Internal => self.internal = true,
        }
    }

    pub(crate) fn record_planning(&mut self, error: &PlanningError) {
        self.transient |= *error == PlanningError::TemporarilyUnavailable;
        self.internal |= *error == PlanningError::Internal;
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.transient |= other.transient;
        self.internal |= other.internal;
    }
}
