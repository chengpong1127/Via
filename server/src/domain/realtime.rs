use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime, PrimitiveDateTime, Time};

use super::ids::{BoardingPointId, TransitPatternId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    Fresh,
    Degraded,
    Unavailable,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Freshness {
    pub state: FreshnessState,
    pub reason: Option<FreshnessReason>,
    pub policy_version: String,
    #[serde(with = "time::serde::rfc3339")]
    pub evaluated_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrivalState {
    Predicted,
    AtBoardingPoint,
    NotDeparted,
    Skipped,
    LastServicePassed,
    NotOperating,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrivalEstimate {
    pub transit_pattern_id: TransitPatternId,
    pub boarding_point_id: BoardingPointId,
    pub predicted_arrival_at: Option<OffsetDateTime>,
    pub state: ArrivalState,
    pub transmitted_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RealtimeValue {
    ArrivalEstimate(ArrivalEstimate),
    ServiceCondition(ServiceCondition),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRealtimeValue {
    pub id: String,
    pub received_at: OffsetDateTime,
    pub normalized_at: OffsetDateTime,
    pub provider_published_at: Option<OffsetDateTime>,
    pub source_change_at: Option<OffsetDateTime>,
    pub freshness: Freshness,
    pub value: RealtimeValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceConditionState {
    Disrupted,
    Suspended,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceCondition {
    pub state: ServiceConditionState,
    pub scope_via_ids: Vec<String>,
    pub source_text_variants: Vec<LocalizedSourceText>,
    pub effective_from: Option<OffsetDateTime>,
    pub effective_until: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizedSourceText {
    pub locale: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceDayPolicy {
    boundary: Time,
}

impl ServiceDayPolicy {
    pub fn new(boundary: Time) -> Self {
        Self { boundary }
    }

    pub fn resolve(self, local_time: PrimitiveDateTime) -> Date {
        if local_time.time() < self.boundary {
            local_time
                .date()
                .previous_day()
                .unwrap_or(local_time.date())
        } else {
            local_time.date()
        }
    }
}
