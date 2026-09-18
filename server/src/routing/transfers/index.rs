//! Static public-transit interchange shortcuts.
//!
//! A transfer connects one public-transit boarding point to another through a
//! static walking/interchange edge.

use std::collections::HashMap;

use crate::domain::{ids::BoardingPointId, transit::TransitNetwork};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferShortcut {
    pub from: BoardingPointId,
    pub to: BoardingPointId,
    pub duration_seconds: u32,
}

impl TransferShortcut {
    pub fn new(from: BoardingPointId, to: BoardingPointId, duration_seconds: u32) -> Self {
        Self {
            from,
            to,
            duration_seconds,
        }
    }
}

/// Lookup seam used by the static walking resolver.
pub trait TransferIndex: Send + Sync {
    fn transfers_from(&self, stop: &BoardingPointId) -> &[TransferShortcut];
}

/// The in-memory index of static public-transit interchange edges.
#[derive(Clone, Debug, Default)]
pub struct TransferShortcutIndex {
    by_from: HashMap<BoardingPointId, Vec<TransferShortcut>>,
}

impl TransferShortcutIndex {
    pub fn new(shortcuts: impl IntoIterator<Item = TransferShortcut>) -> Self {
        let mut index = Self::default();
        for shortcut in shortcuts {
            index.insert_unsorted(shortcut);
        }
        index.sort();
        index
    }

    pub fn from_network(network: &TransitNetwork) -> Self {
        Self::new(network.transfer_edges.iter().map(|edge| {
            TransferShortcut::new(edge.from.clone(), edge.to.clone(), edge.duration_seconds)
        }))
    }

    fn insert_unsorted(&mut self, shortcut: TransferShortcut) {
        let alternatives = self.by_from.entry(shortcut.from.clone()).or_default();
        if !alternatives.contains(&shortcut) {
            alternatives.push(shortcut);
        }
    }

    pub fn len(&self) -> usize {
        self.by_from.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_from.values().all(Vec::is_empty)
    }

    fn sort(&mut self) {
        for alternatives in self.by_from.values_mut() {
            alternatives.sort_by(|left, right| {
                left.to
                    .as_str()
                    .cmp(right.to.as_str())
                    .then(left.duration_seconds.cmp(&right.duration_seconds))
            });
        }
    }
}

impl TransferIndex for TransferShortcutIndex {
    fn transfers_from(&self, stop: &BoardingPointId) -> &[TransferShortcut] {
        self.by_from.get(stop).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::transit::{TransferEdge, TransitNetwork, TransitStop},
    };

    fn stop(id: &str) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(id),
            id,
            RequestCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )
    }

    #[test]
    fn from_network_projects_only_static_transit_edges() {
        let network = TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("a"), stop("b")],
            Vec::new(),
            Vec::new(),
        )
        .with_transfer_edges(vec![
            TransferEdge::new(BoardingPointId::new("a"), BoardingPointId::new("b"), 90),
            TransferEdge::new(BoardingPointId::new("b"), BoardingPointId::new("a"), 90),
        ]);

        let index = TransferShortcutIndex::from_network(&network);
        assert_eq!(index.len(), 2);
        assert_eq!(
            index.transfers_from(&BoardingPointId::new("a"))[0].duration_seconds,
            90
        );
    }
}
