use crate::{domain::transit_timing::DepartureError, live::UnavailableReason};

pub(super) fn departure_error(reason: UnavailableReason) -> DepartureError {
    match reason {
        UnavailableReason::RateLimited => DepartureError::RateLimited,
        UnavailableReason::TimedOut => DepartureError::TimedOut,
        UnavailableReason::UpstreamUnavailable => DepartureError::TemporarilyUnavailable,
        UnavailableReason::InvalidResponse => DepartureError::InvalidResponse,
        UnavailableReason::InvalidConfiguration => DepartureError::InvalidConfiguration,
    }
}
