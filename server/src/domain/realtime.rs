use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use super::ids::{BoardingPointId, TransitPatternId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    Fresh,
    Degraded,
    Unavailable,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveStatusTimes {
    pub published_at: Option<OffsetDateTime>,
    pub received_at: OffsetDateTime,
}

impl LiveStatusTimes {
    pub fn received(received_at: OffsetDateTime) -> Self {
        Self {
            published_at: None,
            received_at,
        }
    }

    pub(crate) fn evidence_time(self) -> OffsetDateTime {
        self.published_at.unwrap_or(self.received_at)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshnessPolicy {
    pub fresh_through: Duration,
    pub degraded_through: Duration,
}

impl FreshnessPolicy {
    pub fn tdx_default() -> Self {
        Self {
            fresh_through: Duration::seconds(120),
            degraded_through: Duration::seconds(180),
        }
    }

    pub fn evaluate(&self, times: LiveStatusTimes, evaluated_at: OffsetDateTime) -> FreshnessState {
        let age = evaluated_at - times.evidence_time();
        if age <= self.fresh_through {
            FreshnessState::Fresh
        } else if age <= self.degraded_through {
            FreshnessState::Degraded
        } else {
            FreshnessState::Unavailable
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalState {
    Predicted,
    AtBoardingPoint,
    NotDeparted,
    Skipped,
    LastServicePassed,
    NotOperating,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrivalEstimate {
    pub transit_pattern_id: TransitPatternId,
    pub boarding_point_id: BoardingPointId,
    pub predicted_arrival_at: Option<OffsetDateTime>,
    pub state: ArrivalState,
    pub transmitted_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedRealtimeValue {
    pub freshness: FreshnessState,
    pub value: ArrivalEstimate,
}
