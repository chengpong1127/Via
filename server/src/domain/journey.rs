use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{location::GeoCoordinate, realtime::FreshnessState, transit::TransitMode};

pub use super::ranking::JourneyLabel;

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
#[schema(as = JourneyMode)]
pub enum LegMode {
    Walking,
    Transit,
    Bicycle,
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
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub journey_options: Vec<JourneyOption>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct EmptyResult {
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub empty: EmptyReason,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct JourneyOption {
    pub id: String,
    pub matching_key: String,
    pub label: Option<JourneyLabel>,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    pub duration_seconds: u32,
    pub walking_seconds: u32,
    pub transfer_count: u32,
    pub travel_modes: Vec<LegMode>,
    pub live_status_summary: LiveStatusSummary,
    pub overview_geometry: Geometry,
    pub legs: Vec<Leg>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Geometry {
    pub encoding: &'static str,
    pub points: String,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Leg {
    pub id: String,
    pub mode: LegMode,
    pub from: Place,
    pub to: Place,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    pub duration_seconds: u32,
    pub geometry: Geometry,
    pub guidance: Vec<Guidance>,
    pub details: LegDetails,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Place {
    pub display_name: String,
    pub coordinate: GeoCoordinate,
    pub via_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct StopVisit {
    #[serde(flatten)]
    pub place: Place,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Guidance {
    pub code: &'static str,
    pub parameters: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct LiveStatusSummary {
    pub state: FreshnessState,
    #[serde(with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LegDetails {
    Walking,
    Transit {
        transit_mode: TransitMode,
        via_pattern_id: String,
        via_boarding_point_id: String,
        via_alighting_point_id: String,
        service_label: String,
        headsign: String,
        intermediate_stops: Vec<StopVisit>,
    },
    BikeShare {
        pickup_station_id: String,
        pickup_station_name: String,
        dropoff_station_id: String,
        dropoff_station_name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveStatusEvidence {
    pub state: FreshnessState,
    pub observed_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct EmptyReason {
    pub code: &'static str,
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
