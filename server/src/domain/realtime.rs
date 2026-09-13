//! Provider-neutral realtime facts and freshness-aware value types.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

use super::ids::{BikeShareStationId, BoardingPointId, TransitServiceId};

pub use crate::live_status::{Freshness, FreshnessReason, FreshnessState};

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
    pub transit_service_id: TransitServiceId,
    pub boarding_point_id: BoardingPointId,
    pub predicted_arrival_at: Option<OffsetDateTime>,
    pub state: ArrivalState,
    pub transmitted_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionState {
    Enabled,
    Disabled,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BikeAvailability {
    pub station_id: BikeShareStationId,
    pub rental_state: ActionState,
    pub return_state: ActionState,
    pub available_bikes: Option<u32>,
    pub available_return_slots: Option<u32>,
    pub source_change_at: Option<OffsetDateTime>,
    pub provider_published_at: Option<OffsetDateTime>,
    pub received_at: OffsetDateTime,
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

pub use crate::realtime::{
    ArrivalQuery, CachedRealtimeProvider, NormalizedRealtimeValue, NullRealtimeProvider,
    RealtimeError, RealtimeMetrics, RealtimeProvider, RealtimeValue, RealtimeValueKind,
};
