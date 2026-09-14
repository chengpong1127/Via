//! Convert and aggregate realtime evidence used by reconstructed journeys.

use time::OffsetDateTime;

use crate::domain::{
    journey::{EvidenceType, JourneyMode, LiveStatusEvidence, LiveStatusKind, LiveStatusSummary},
    ranking::TransferKind,
    realtime::{FreshnessState, NormalizedRealtimeValue, RealtimeValue},
    transit::TransitMode,
};

pub(crate) fn from_realtime_value(status: &NormalizedRealtimeValue) -> LiveStatusEvidence {
    let (kind, observed_at) = match &status.value {
        RealtimeValue::ArrivalEstimate(arrival) => (
            LiveStatusKind::ArrivalEstimate,
            Some(arrival.transmitted_at),
        ),
        RealtimeValue::ServiceCondition(_) => (
            LiveStatusKind::ServiceCondition,
            status.provider_published_at,
        ),
        RealtimeValue::BikeAvailability(availability) => (
            LiveStatusKind::BikeAvailability,
            bike_observed_at(availability),
        ),
    };
    LiveStatusEvidence {
        kind,
        evidence_type: EvidenceType::Live,
        observed_at,
        published_at: match &status.value {
            RealtimeValue::ArrivalEstimate(arrival) => Some(arrival.transmitted_at),
            RealtimeValue::BikeAvailability(availability) => availability.provider_published_at,
            RealtimeValue::ServiceCondition(_) => status.provider_published_at,
        }
        .or(status.provider_published_at),
        received_at: status.received_at,
        evaluated_at: status.freshness.evaluated_at,
        freshness: status.freshness.clone(),
    }
}

pub(crate) fn push_unique_mode(modes: &mut Vec<JourneyMode>, mode: JourneyMode) {
    if !modes.contains(&mode) {
        modes.push(mode);
    }
}

pub(crate) fn transfer_kind(from: Option<TransitMode>, to: Option<TransitMode>) -> TransferKind {
    if from == Some(TransitMode::Metro) && to == Some(TransitMode::Metro) {
        TransferKind::MetroToMetro
    } else {
        TransferKind::BusInvolved
    }
}

fn bike_observed_at(
    availability: &crate::domain::realtime::BikeAvailability,
) -> Option<OffsetDateTime> {
    availability
        .provider_published_at
        .or(availability.source_change_at)
        .or(Some(availability.received_at))
}

pub(crate) fn summary_from_evidence(evidence: &[LiveStatusEvidence]) -> LiveStatusSummary {
    if evidence.is_empty() {
        return LiveStatusSummary {
            state: FreshnessState::NotApplicable,
            reasons: Vec::new(),
            observed_at: None,
        };
    }
    let mut reasons = evidence
        .iter()
        .filter_map(|evidence| evidence.freshness.reason)
        .collect::<Vec<_>>();
    reasons.sort_by_key(|reason| format!("{reason:?}"));
    reasons.dedup();
    let state = if evidence
        .iter()
        .any(|evidence| evidence.freshness.state == FreshnessState::Unavailable)
    {
        FreshnessState::Unavailable
    } else if evidence
        .iter()
        .any(|evidence| evidence.freshness.state == FreshnessState::Degraded)
    {
        FreshnessState::Degraded
    } else if evidence
        .iter()
        .any(|evidence| evidence.freshness.state == FreshnessState::Fresh)
    {
        FreshnessState::Fresh
    } else {
        FreshnessState::NotApplicable
    };
    LiveStatusSummary {
        state,
        reasons,
        observed_at: evidence
            .iter()
            .filter_map(|evidence| evidence.observed_at)
            .min(),
    }
}
