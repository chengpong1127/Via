//! Canonical provider-neutral journey and routing result types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{
    location::RequestCoordinate,
    realtime::{Freshness, FreshnessReason, FreshnessState},
    transit::{NetworkRevision, TransitMode},
};

pub use super::ranking::{JourneyClass, JourneyLabel, RankingReason};

#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum JourneyMode {
    Walking,
    Transit,
    Bicycle,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum PlanningOutcome {
    Options(OptionsResult),
    Empty(EmptyResult),
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JourneyOptionsResult)]
pub struct OptionsResult {
    #[serde(flatten)]
    pub metadata: ResultMetadata,
    pub journey_options: Vec<JourneyOption>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct EmptyResult {
    #[serde(flatten)]
    pub metadata: ResultMetadata,
    pub empty: SemanticObject,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ResultMetadata {
    pub api_version: &'static str,
    pub semantic_catalog_version: &'static str,
    pub request_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub effective_search_time: OffsetDateTime,
    pub time_zone: &'static str,
    pub network_revision: NetworkRevision,
    pub policy_versions: PolicyVersions,
    pub warnings: Vec<Warning>,
}

impl ResultMetadata {
    pub fn new(
        request_id: String,
        generated_at: OffsetDateTime,
        effective_search_time: OffsetDateTime,
        network_revision: NetworkRevision,
    ) -> Self {
        Self {
            api_version: "1.0",
            semantic_catalog_version: "1.0",
            request_id,
            generated_at,
            effective_search_time,
            time_zone: "Asia/Taipei",
            network_revision,
            policy_versions: PolicyVersions {
                freshness: "1".into(),
                ranking: "1".into(),
                transfer_safety: "1".into(),
            },
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PolicyVersions {
    pub freshness: String,
    pub ranking: String,
    pub transfer_safety: String,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct JourneyOption {
    pub id: String,
    pub matching_key: String,
    pub class: JourneyClass,
    pub label: Option<JourneyLabel>,
    pub ranking_reasons: Vec<RankingReason>,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    pub duration_seconds: u32,
    pub walking_seconds: u32,
    pub bicycle_seconds: u32,
    pub bike_share_uses: u32,
    pub transfer_count: u32,
    pub travel_modes: Vec<JourneyMode>,
    pub live_status_summary: LiveStatusSummary,
    pub overview_geometry: Geometry,
    pub legs: Vec<Leg>,
    pub transfers: Vec<Transfer>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Geometry {
    pub encoding: &'static str,
    pub points: String,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Leg {
    pub id: String,
    pub mode: JourneyMode,
    pub from: Place,
    pub to: Place,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_departure_time: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_arrival_time: Option<OffsetDateTime>,
    pub duration_seconds: u32,
    pub distance_metres: u32,
    pub geometry: Geometry,
    pub guidance: Vec<Guidance>,
    pub details: LegDetails,
    pub live_status: Vec<LiveStatusEvidence>,
    pub warnings: Vec<Warning>,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Place {
    pub display_name: String,
    pub coordinate: RequestCoordinate,
    pub kind: &'static str,
    pub via_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Guidance {
    pub code: &'static str,
    pub required: bool,
    pub parameters: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Transfer {
    pub from_leg_id: String,
    pub to_leg_id: String,
    pub connector_leg_ids: Vec<String>,
    pub duration_seconds: u32,
    pub available_seconds: u32,
    pub class: JourneyClass,
    pub reasons: Vec<RankingReason>,
    pub geometry: Option<Geometry>,
    pub guidance: Vec<Guidance>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct LiveStatusSummary {
    pub state: FreshnessState,
    pub reasons: Vec<FreshnessReason>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Warning {
    pub code: &'static str,
    pub required: bool,
    pub severity: &'static str,
    pub parameters: BTreeMap<String, serde_json::Value>,
    pub scope: WarningScope,
    pub source_text_variants: Vec<SourceTextVariant>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LegDetails {
    Walking {
        start_fact: Option<String>,
        end_fact: Option<String>,
    },
    Transit {
        transit_mode: TransitMode,
        via_service_id: String,
        via_boarding_point_id: String,
        via_alighting_point_id: String,
        service_label: String,
        headsign: String,
        stop_count: Option<u32>,
    },
    BikeShare {
        pickup_station_id: String,
        pickup_station_name: String,
        dropoff_station_id: String,
        dropoff_station_name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct LiveStatusEvidence {
    pub kind: LiveStatusKind,
    pub evidence_type: EvidenceType,
    #[serde(with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub evaluated_at: OffsetDateTime,
    pub freshness: Freshness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatusKind {
    ArrivalEstimate,
    ServiceCondition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    Live,
    ScheduleOnly,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct WarningScope {
    #[serde(rename = "type")]
    pub kind: WarningScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningScopeKind {
    Result,
    JourneyOption,
    Leg,
    Transfer,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct SourceTextVariant {
    pub locale: String,
    pub text: String,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct SemanticObject {
    pub code: &'static str,
    pub required: bool,
    pub parameters: BTreeMap<&'static str, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Attribution {
    pub text: String,
    pub url: String,
    pub licence: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanningError {
    TemporarilyUnavailable,
    Internal,
}

impl PlanningError {
    pub fn is_temporary(&self) -> bool {
        matches!(self, Self::TemporarilyUnavailable)
    }
}
