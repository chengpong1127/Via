use super::super::{AccessPoint, JourneySegment};
use crate::domain::{
    ids::{BoardingPointId, TransitServiceId},
    realtime::NormalizedRealtimeValue,
    transit_timing::{DepartureQuery, EffectiveTrip, TimingBasis, TimingReliability},
};
use std::sync::Arc;
use time::OffsetDateTime;

#[derive(Clone)]
pub(super) struct SearchPath {
    pub(super) access: AccessPoint,
    pub(super) segments: Vec<JourneySegment>,
    pub(super) walking_seconds: u32,
    pub(super) transfer_count: u32,
    pub(super) evidence: Vec<NormalizedRealtimeValue>,
    pub(super) timing_bases: Vec<TimingBasis>,
    pub(super) timing_reliability: TimingReliability,
    pub(super) uncertainty_seconds: Option<u32>,
}

impl SearchPath {
    pub(super) fn from_access(access: AccessPoint) -> Self {
        Self {
            walking_seconds: access.walking_seconds,
            access,
            segments: Vec::new(),
            transfer_count: 0,
            evidence: Vec::new(),
            timing_bases: Vec::new(),
            timing_reliability: TimingReliability::Reliable,
            uncertainty_seconds: None,
        }
    }

    pub(super) fn with_transfer(&self, segment: JourneySegment, walking_seconds: u32) -> Self {
        let mut next = self.clone();
        next.segments.push(segment);
        next.walking_seconds = next.walking_seconds.saturating_add(walking_seconds);
        next
    }

    pub(super) fn with_trip_timing(&self, trip: &EffectiveTrip, service_changed: bool) -> Self {
        let mut next = self.clone();
        if service_changed {
            next.transfer_count = next.transfer_count.saturating_add(1);
        }
        next.evidence.extend(trip.evidence.clone());
        next.timing_bases.push(trip.timing_basis);
        if trip.timing_reliability == TimingReliability::Fallback {
            next.timing_reliability = TimingReliability::Fallback;
        }
        next.uncertainty_seconds = match (next.uncertainty_seconds, trip.uncertainty_seconds) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (None, value) | (value, None) => value,
        };
        next
    }
}

#[derive(Clone)]
pub(super) struct BoardingNeed {
    pub(super) stop_id: BoardingPointId,
    pub(super) ready_at: OffsetDateTime,
    pub(super) round: usize,
    pub(super) previous_service: Option<TransitServiceId>,
    pub(super) excluded_service: Option<TransitServiceId>,
    pub(super) path: SearchPath,
}

#[derive(Clone)]
pub(super) struct TripState {
    pub(super) trip: Arc<EffectiveTrip>,
    pub(super) board_index: usize,
    pub(super) round: usize,
    pub(super) path: Arc<SearchPath>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct DepartureCacheKey(pub(super) DepartureQuery);

#[derive(Clone, Copy)]
pub(super) struct ReachedTripLabel {
    pub(super) board_index: usize,
    pub(super) departure_at: OffsetDateTime,
    pub(super) transfer_count: u32,
    pub(super) walking_seconds: u32,
}

#[derive(Clone, Copy)]
pub(super) struct DynamicLabel {
    pub(super) ready_at: OffsetDateTime,
    pub(super) transfer_count: u32,
    pub(super) walking_seconds: u32,
}
