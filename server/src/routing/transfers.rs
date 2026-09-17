//! Algorithm-neutral public-transit interchange shortcuts.
//!
//! A transfer connects one public-transit boarding point to another through a
//! static walking/interchange edge. Query-time BikeShare paths are endpoint
//! connectors and live in [`super::bike`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::{ids::BoardingPointId, transit::TransitNetwork};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferLeg {
    Walk {
        duration_seconds: u32,
        distance_metres: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPath {
    pub legs: Vec<TransferLeg>,
}

impl TransferPath {
    pub fn walking(duration_seconds: u32, distance_metres: u32) -> Self {
        Self {
            legs: vec![TransferLeg::Walk {
                duration_seconds,
                distance_metres,
            }],
        }
    }

    pub fn duration_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                TransferLeg::Walk {
                    duration_seconds, ..
                } => *duration_seconds,
            })
        })
    }

    pub fn walking_seconds(&self) -> u32 {
        self.duration_seconds()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferShortcut {
    pub from: BoardingPointId,
    pub to: BoardingPointId,
    pub duration_seconds: u32,
    pub walking_seconds: u32,
    pub path: TransferPath,
}

impl TransferShortcut {
    pub fn walking(
        from: BoardingPointId,
        to: BoardingPointId,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Self {
        Self::from_path(
            from,
            to,
            TransferPath::walking(duration_seconds, distance_metres),
        )
    }

    pub fn from_path(from: BoardingPointId, to: BoardingPointId, path: TransferPath) -> Self {
        let duration_seconds = path.duration_seconds();
        Self {
            from,
            to,
            duration_seconds,
            walking_seconds: path.walking_seconds(),
            path,
        }
    }

    fn signature(&self) -> String {
        format!(
            "{}:{}:{}",
            self.from.as_str(),
            self.to.as_str(),
            self.duration_seconds
        )
    }
}

/// Shared transfer seam for Trip-Based, RAPTOR, CSA, and future algorithms.
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
            TransferShortcut::walking(edge.from.clone(), edge.to.clone(), edge.duration_seconds, 0)
        }))
    }

    pub fn insert(&mut self, shortcut: TransferShortcut) {
        self.insert_unsorted(shortcut);
        self.sort();
    }

    fn insert_unsorted(&mut self, shortcut: TransferShortcut) {
        let alternatives = self.by_from.entry(shortcut.from.clone()).or_default();
        if !alternatives.contains(&shortcut) {
            alternatives.push(shortcut);
        }
    }

    pub fn extend(&mut self, shortcuts: impl IntoIterator<Item = TransferShortcut>) {
        for shortcut in shortcuts {
            self.insert_unsorted(shortcut);
        }
        self.sort();
    }

    pub fn len(&self) -> usize {
        self.by_from.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_from.values().all(Vec::is_empty)
    }

    pub fn shortcuts(&self) -> Vec<TransferShortcut> {
        let mut shortcuts = self
            .by_from
            .values()
            .flat_map(|alternatives| alternatives.iter().cloned())
            .collect::<Vec<_>>();
        shortcuts.sort_by(|left, right| {
            left.from
                .as_str()
                .cmp(right.from.as_str())
                .then(left.to.as_str().cmp(right.to.as_str()))
                .then(left.duration_seconds.cmp(&right.duration_seconds))
                .then(left.signature().cmp(&right.signature()))
        });
        shortcuts
    }

    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&TransferShortcutArtifact {
            schema_version: 1,
            shortcuts: self.shortcuts(),
        })
    }

    pub fn from_artifact_json(input: &str) -> Result<Self, serde_json::Error> {
        let artifact = serde_json::from_str::<TransferShortcutArtifact>(input)?;
        Ok(Self::new(artifact.shortcuts))
    }

    pub fn validate_against(&self, network: &TransitNetwork) -> Result<(), String> {
        for shortcut in self.shortcuts() {
            if shortcut.from == shortcut.to {
                return Err(format!(
                    "transfer shortcut {} -> {} is a self-transfer",
                    shortcut.from.as_str(),
                    shortcut.to.as_str()
                ));
            }
            if network.stop(&shortcut.from).is_none() || network.stop(&shortcut.to).is_none() {
                return Err(format!(
                    "transfer shortcut references unknown endpoint {} -> {}",
                    shortcut.from.as_str(),
                    shortcut.to.as_str()
                ));
            }
            if shortcut.duration_seconds == 0
                || shortcut.path.legs.iter().any(|leg| match leg {
                    TransferLeg::Walk {
                        duration_seconds, ..
                    } => *duration_seconds == 0,
                })
            {
                return Err(format!(
                    "transfer shortcut {} -> {} has a non-positive duration",
                    shortcut.from.as_str(),
                    shortcut.to.as_str()
                ));
            }
        }
        Ok(())
    }

    fn sort(&mut self) {
        for alternatives in self.by_from.values_mut() {
            alternatives.sort_by(|left, right| {
                left.to
                    .as_str()
                    .cmp(right.to.as_str())
                    .then(left.duration_seconds.cmp(&right.duration_seconds))
                    .then(left.walking_seconds.cmp(&right.walking_seconds))
            });
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferShortcutArtifact {
    pub schema_version: u32,
    pub shortcuts: Vec<TransferShortcut>,
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
        assert!(index.shortcuts().iter().all(|shortcut| {
            shortcut
                .path
                .legs
                .iter()
                .all(|leg| matches!(leg, TransferLeg::Walk { .. }))
        }));
    }
}
