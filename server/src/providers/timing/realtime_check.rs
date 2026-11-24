use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        realtime::{FreshnessPolicy, FreshnessState, LiveStatusTimes},
        transit::timing::DepartureError,
    },
    tdx::RealtimeSettings,
};

#[derive(Clone, Debug)]
pub(super) struct RealtimeCheck {
    freshness: FreshnessPolicy,
    max_clock_skew: Duration,
}

impl RealtimeCheck {
    pub(super) fn new(realtime: &RealtimeSettings) -> Self {
        Self {
            freshness: realtime.freshness_policy(),
            max_clock_skew: Duration::seconds(i64::from(realtime.max_clock_skew_seconds)),
        }
    }

    #[cfg(test)]
    pub(super) fn with_policy(freshness: FreshnessPolicy, max_clock_skew_seconds: u32) -> Self {
        Self {
            freshness,
            max_clock_skew: Duration::seconds(i64::from(max_clock_skew_seconds)),
        }
    }

    pub(super) fn reject_future(
        &self,
        timestamp: OffsetDateTime,
        received_at: OffsetDateTime,
    ) -> Result<(), DepartureError> {
        if timestamp > received_at + self.max_clock_skew {
            return Err(DepartureError::InvalidResponse);
        }
        Ok(())
    }

    pub(super) fn freshness_of(
        &self,
        published_at: OffsetDateTime,
        received_at: OffsetDateTime,
    ) -> FreshnessState {
        self.freshness.evaluate(
            LiveStatusTimes {
                published_at: Some(published_at),
                received_at,
            },
            received_at,
        )
    }
}

pub(super) fn parse_timestamp(value: &str) -> Result<OffsetDateTime, DepartureError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| DepartureError::InvalidResponse)
}
