use async_trait::async_trait;
use time::Duration;

use super::{
    ResolvedTransfer, TransferIndex, TransferNeed, TransferResolver, TransferShortcutIndex,
    TransferTarget,
};
use crate::routing::JourneySegment;

#[async_trait]
impl TransferResolver for TransferShortcutIndex {
    async fn expand_batch(&self, needs: &[TransferNeed]) -> Vec<ResolvedTransfer> {
        needs
            .iter()
            .enumerate()
            .flat_map(|(input_index, need)| {
                self.transfers_from(&need.source)
                    .iter()
                    .filter(move |shortcut| shortcut.to != need.source)
                    .filter_map(move |shortcut| {
                        ResolvedTransfer::new(
                            input_index,
                            need,
                            TransferTarget::BoardingPoint(shortcut.to.clone()),
                            need.ready_at + Duration::seconds(i64::from(shortcut.duration_seconds)),
                            JourneySegment::WalkingTransfer {
                                shortcut: shortcut.clone(),
                            },
                            shortcut.duration_seconds,
                        )
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use time::{Duration, macros::datetime};

    use super::*;
    use crate::domain::ids::BoardingPointId;
    use crate::routing::transfers::TransferShortcut;

    #[tokio::test]
    async fn resolves_multiple_sources_and_alternatives_in_one_batch() {
        let index = TransferShortcutIndex::new([
            TransferShortcut::new(BoardingPointId::new("a"), BoardingPointId::new("b"), 30),
            TransferShortcut::new(BoardingPointId::new("a"), BoardingPointId::new("c"), 50),
            TransferShortcut::new(BoardingPointId::new("d"), BoardingPointId::new("e"), 70),
        ]);
        let ready_at = datetime!(2026-08-31 10:00 UTC);
        let resolved = index
            .expand_batch(&[
                TransferNeed {
                    source: BoardingPointId::new("a"),
                    ready_at,
                    round: 1,
                },
                TransferNeed {
                    source: BoardingPointId::new("d"),
                    ready_at,
                    round: 1,
                },
            ])
            .await;
        assert_eq!(resolved.len(), 3);
        assert_eq!(
            resolved
                .iter()
                .map(ResolvedTransfer::input_index)
                .collect::<Vec<_>>(),
            vec![0, 0, 1]
        );
        assert_eq!(
            resolved[0].target(),
            &TransferTarget::BoardingPoint(BoardingPointId::new("b"))
        );
        assert_eq!(
            resolved[1].target(),
            &TransferTarget::BoardingPoint(BoardingPointId::new("c"))
        );
        assert_eq!(
            resolved[2].target(),
            &TransferTarget::BoardingPoint(BoardingPointId::new("e"))
        );
        assert_eq!(resolved[1].arrival_at(), ready_at + Duration::seconds(50));
        assert!(matches!(
            resolved[1].segment(),
            JourneySegment::WalkingTransfer { .. }
        ));
        assert_eq!(resolved[1].walking_seconds(), 50);
    }

    #[test]
    fn inconsistent_transfer_facts_cannot_be_constructed() {
        let need = TransferNeed {
            source: BoardingPointId::new("a"),
            ready_at: datetime!(2026-08-31 10:00 UTC),
            round: 1,
        };
        let segment = JourneySegment::WalkingTransfer {
            shortcut: TransferShortcut::new(
                BoardingPointId::new("a"),
                BoardingPointId::new("b"),
                30,
            ),
        };
        assert!(
            ResolvedTransfer::new(
                0,
                &need,
                TransferTarget::BoardingPoint(BoardingPointId::new("c")),
                need.ready_at + Duration::seconds(30),
                segment.clone(),
                30,
            )
            .is_none()
        );
        assert!(
            ResolvedTransfer::new(
                0,
                &need,
                TransferTarget::BoardingPoint(BoardingPointId::new("b")),
                need.ready_at + Duration::seconds(40),
                segment.clone(),
                30,
            )
            .is_none()
        );
        assert!(
            ResolvedTransfer::new(
                0,
                &need,
                TransferTarget::BoardingPoint(BoardingPointId::new("b")),
                need.ready_at + Duration::seconds(30),
                segment,
                40,
            )
            .is_none()
        );
    }
}
