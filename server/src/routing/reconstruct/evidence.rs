use crate::domain::{
    journey::{LegMode, LiveStatusEvidence, LiveStatusSummary},
    ranking::TransferKind,
    realtime::{FreshnessState, NormalizedRealtimeValue},
    transit::TransitMode,
};

pub(super) fn from_realtime_value(status: &NormalizedRealtimeValue) -> LiveStatusEvidence {
    LiveStatusEvidence {
        state: status.freshness,
        observed_at: Some(status.value.transmitted_at),
    }
}

pub(super) fn push_unique_mode(modes: &mut Vec<LegMode>, mode: LegMode) {
    if !modes.contains(&mode) {
        modes.push(mode);
    }
}

pub(super) fn transfer_kind(from: Option<TransitMode>, to: Option<TransitMode>) -> TransferKind {
    if from == Some(TransitMode::Metro) && to == Some(TransitMode::Metro) {
        TransferKind::MetroToMetro
    } else {
        TransferKind::BusInvolved
    }
}

pub(super) fn summary_from_evidence(evidence: &[LiveStatusEvidence]) -> LiveStatusSummary {
    if evidence.is_empty() {
        return LiveStatusSummary {
            state: FreshnessState::NotApplicable,
            observed_at: None,
        };
    }
    let state = if evidence
        .iter()
        .any(|evidence| evidence.state == FreshnessState::Unavailable)
    {
        FreshnessState::Unavailable
    } else if evidence
        .iter()
        .any(|evidence| evidence.state == FreshnessState::Degraded)
    {
        FreshnessState::Degraded
    } else if evidence
        .iter()
        .any(|evidence| evidence.state == FreshnessState::Fresh)
    {
        FreshnessState::Fresh
    } else {
        FreshnessState::NotApplicable
    };
    LiveStatusSummary {
        state,
        observed_at: evidence
            .iter()
            .filter_map(|evidence| evidence.observed_at)
            .min(),
    }
}
