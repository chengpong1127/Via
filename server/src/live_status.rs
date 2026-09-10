use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    Fresh,
    Degraded,
    Unavailable,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessReason {
    Stale,
    Missing,
    ScheduleOnly,
    OperatorNotCovered,
    RateLimited,
    SourceError,
    RejectedPayload,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freshness {
    pub state: FreshnessState,
    pub reason: Option<FreshnessReason>,
    pub policy_version: String,
    #[serde(with = "time::serde::rfc3339")]
    pub evaluated_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveStatusTimes {
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub measured_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub normalized_at: OffsetDateTime,
}

impl LiveStatusTimes {
    pub fn received(received_at: OffsetDateTime) -> Self {
        Self {
            measured_at: None,
            published_at: None,
            received_at,
            normalized_at: received_at,
        }
    }

    pub(crate) fn evidence_time(self) -> OffsetDateTime {
        self.published_at.unwrap_or(self.received_at)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshnessPolicy {
    pub version: String,
    pub fresh_through: Duration,
    pub degraded_through: Duration,
}

impl FreshnessPolicy {
    pub fn tdx_one_minute(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            fresh_through: Duration::seconds(120),
            degraded_through: Duration::seconds(180),
        }
    }

    pub fn evaluate(
        &self,
        times: LiveStatusTimes,
        evaluated_at: OffsetDateTime,
        source_failure: Option<FreshnessReason>,
    ) -> Freshness {
        if let Some(reason) = source_failure {
            return Freshness {
                state: FreshnessState::Unavailable,
                reason: Some(reason),
                policy_version: self.version.clone(),
                evaluated_at,
            };
        }

        let age = evaluated_at - times.evidence_time();
        let (state, reason) = if age <= self.fresh_through {
            (FreshnessState::Fresh, None)
        } else if age <= self.degraded_through {
            (FreshnessState::Degraded, Some(FreshnessReason::Stale))
        } else {
            (FreshnessState::Unavailable, Some(FreshnessReason::Stale))
        };
        Freshness {
            state,
            reason,
            policy_version: self.version.clone(),
            evaluated_at,
        }
    }
}
