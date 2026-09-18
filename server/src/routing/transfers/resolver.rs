use async_trait::async_trait;
use time::Duration;
use time::OffsetDateTime;

use crate::domain::ids::BoardingPointId;

use super::super::JourneySegment;

/// A movement opportunity collected during a transit search round.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TransferNeed {
    pub source: BoardingPointId,
    pub ready_at: OffsetDateTime,
    pub round: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferTarget {
    BoardingPoint(BoardingPointId),
}

/// The input index preserves the search path when equal physical requests share
/// one batched lookup. The segment is carried through without mode inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTransfer {
    input_index: usize,
    need: TransferNeed,
    target: TransferTarget,
    arrival_at: OffsetDateTime,
    segment: JourneySegment,
    walking_seconds: u32,
}

impl ResolvedTransfer {
    /// Keep search facts consistent with the reconstruction segment.
    pub fn new(
        input_index: usize,
        need: &TransferNeed,
        target: TransferTarget,
        arrival_at: OffsetDateTime,
        segment: JourneySegment,
        walking_seconds: u32,
    ) -> Option<Self> {
        let TransferTarget::BoardingPoint(target_id) = &target;
        if target_id == &need.source {
            return None;
        }
        let (segment_source, segment_target, elapsed_seconds, segment_walking_seconds) =
            segment.transfer_facts()?;
        if segment_source != &need.source
            || segment_target != target_id
            || segment_walking_seconds != walking_seconds
            || arrival_at != need.ready_at + Duration::seconds(i64::from(elapsed_seconds))
        {
            return None;
        }
        Some(Self {
            input_index,
            need: need.clone(),
            target,
            arrival_at,
            segment,
            walking_seconds,
        })
    }

    pub fn input_index(&self) -> usize {
        self.input_index
    }
    pub fn source(&self) -> &BoardingPointId {
        &self.need.source
    }
    pub fn need(&self) -> &TransferNeed {
        &self.need
    }
    pub fn target(&self) -> &TransferTarget {
        &self.target
    }
    pub fn arrival_at(&self) -> OffsetDateTime {
        self.arrival_at
    }
    pub fn segment(&self) -> &JourneySegment {
        &self.segment
    }
    pub fn walking_seconds(&self) -> u32 {
        self.walking_seconds
    }

    pub(crate) fn add_input_offset(&mut self, offset: usize) {
        self.input_index += offset;
    }
}

#[async_trait]
pub trait TransferResolver: Send + Sync {
    async fn expand_batch(&self, needs: &[TransferNeed]) -> Vec<ResolvedTransfer>;
}
