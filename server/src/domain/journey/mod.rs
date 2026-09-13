//! Journey result types shared by routing and the HTTP adapter.

use serde::{Deserialize, Serialize};

pub mod ranking;

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
    BikeShare,
    Transit,
    #[serde(other)]
    Unknown,
}

pub use crate::planner::{
    EmptyResult, EvidenceType, Geometry, Guidance, JourneyOption, Leg, LegDetails,
    LiveStatusEvidence, LiveStatusKind, LiveStatusSummary, OptionsResult, Place, PlanningError,
    PlanningOutcome, PolicyVersions, ResultMetadata, SemanticObject, SourceTextVariant, Transfer,
    Warning, WarningScope, WarningScopeKind,
};
pub use ranking::{JourneyClass, JourneyLabel, RankingReason};
