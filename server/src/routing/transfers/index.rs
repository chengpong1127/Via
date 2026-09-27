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

/// The in-memory index of static public-transit interchange edges.
#[derive(Clone, Debug, Default)]
pub struct TransferShortcutIndex {
    by_from: HashMap<BoardingPointId, Vec<TransferShortcut>>,
    by_to: HashMap<BoardingPointId, Vec<TransferShortcut>>,
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
        let alternatives_from = self.by_from.entry(shortcut.from.clone()).or_default();
        if !alternatives_from.contains(&shortcut) {
            alternatives_from.push(shortcut.clone());
            self.by_to
                .entry(shortcut.to.clone())
                .or_default()
                .push(shortcut);
        }
    }

    pub fn transfers_from(&self, stop: &BoardingPointId) -> &[TransferShortcut] {
        self.by_from.get(stop).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn transfers_to(&self, stop: &BoardingPointId) -> &[TransferShortcut] {
        self.by_to.get(stop).map(Vec::as_slice).unwrap_or(&[])
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
        for alternatives in self.by_to.values_mut() {
            alternatives.sort_by(|left, right| {
                left.from
                    .as_str()
                    .cmp(right.from.as_str())
                    .then(left.duration_seconds.cmp(&right.duration_seconds))
            });
        }
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
        let network = TransitNetwork::fixture(
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
        assert_eq!(
            index.transfers_from(&BoardingPointId::new("a"))[0].duration_seconds,
            90
        );
        assert_eq!(
            index.transfers_from(&BoardingPointId::new("b"))[0].to,
            BoardingPointId::new("a")
        );
        assert_eq!(
            index.transfers_to(&BoardingPointId::new("b"))[0].from,
            BoardingPointId::new("a")
        );
        assert_eq!(
            index.transfers_to(&BoardingPointId::new("a"))[0].from,
            BoardingPointId::new("b")
        );
    }
}
