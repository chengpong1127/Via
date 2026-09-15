//! Algorithm-neutral transfer shortcuts.
//!
//! A shortcut is the only representation a transit algorithm needs for
//! moving between boarding points without boarding another transit trip. The
//! path keeps enough information for reconstruction, while runtime bike
//! availability remains an optional query-time constraint.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::domain::{
    ids::{BikeShareStationId, BoardingPointId},
    realtime::BikeAvailability,
    transit::TransitNetwork,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferLeg {
    Walk {
        duration_seconds: u32,
        distance_metres: u32,
    },
    BikeShare {
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        duration_seconds: u32,
        distance_metres: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

    pub fn bike_share(
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Self {
        Self {
            legs: vec![TransferLeg::BikeShare {
                pickup_station_id,
                return_station_id,
                duration_seconds,
                distance_metres,
            }],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn walking_and_bike(
        access_seconds: u32,
        access_metres: u32,
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        bike_seconds: u32,
        bike_metres: u32,
        egress_seconds: u32,
        egress_metres: u32,
    ) -> Self {
        Self {
            legs: vec![
                TransferLeg::Walk {
                    duration_seconds: access_seconds,
                    distance_metres: access_metres,
                },
                TransferLeg::BikeShare {
                    pickup_station_id,
                    return_station_id,
                    duration_seconds: bike_seconds,
                    distance_metres: bike_metres,
                },
                TransferLeg::Walk {
                    duration_seconds: egress_seconds,
                    distance_metres: egress_metres,
                },
            ],
        }
    }

    pub fn duration_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                TransferLeg::Walk {
                    duration_seconds, ..
                }
                | TransferLeg::BikeShare {
                    duration_seconds, ..
                } => *duration_seconds,
            })
        })
    }

    pub fn walking_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                TransferLeg::Walk {
                    duration_seconds, ..
                } => *duration_seconds,
                TransferLeg::BikeShare { .. } => 0,
            })
        })
    }

    pub fn bike_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                TransferLeg::BikeShare {
                    duration_seconds, ..
                } => *duration_seconds,
                TransferLeg::Walk { .. } => 0,
            })
        })
    }

    pub fn required_station_ids(&self) -> Vec<BikeShareStationId> {
        let mut ids = self
            .legs
            .iter()
            .flat_map(|leg| match leg {
                TransferLeg::BikeShare {
                    pickup_station_id,
                    return_station_id,
                    ..
                } => vec![pickup_station_id.clone(), return_station_id.clone()],
                TransferLeg::Walk { .. } => Vec::new(),
            })
            .collect::<Vec<_>>();
        ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ids.dedup();
        ids
    }

    pub fn uses_bike_share(&self) -> bool {
        self.legs
            .iter()
            .any(|leg| matches!(leg, TransferLeg::BikeShare { .. }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferShortcut {
    pub from: BoardingPointId,
    pub to: BoardingPointId,
    pub duration_seconds: u32,
    pub walking_seconds: u32,
    pub bike_seconds: u32,
    pub path: TransferPath,
}

impl TransferShortcut {
    pub fn walking(
        from: BoardingPointId,
        to: BoardingPointId,
        duration_seconds: u32,
        distance_metres: u32,
    ) -> Self {
        let path = TransferPath::walking(duration_seconds, distance_metres);
        Self::from_path(from, to, path)
    }

    pub fn bike_share(from: BoardingPointId, to: BoardingPointId, path: TransferPath) -> Self {
        assert!(
            path.uses_bike_share(),
            "bike shortcut must contain a bike leg"
        );
        Self::from_path(from, to, path)
    }

    pub fn from_path(from: BoardingPointId, to: BoardingPointId, path: TransferPath) -> Self {
        Self {
            from,
            to,
            duration_seconds: path.duration_seconds(),
            walking_seconds: path.walking_seconds(),
            bike_seconds: path.bike_seconds(),
            path,
        }
    }

    pub fn required_station_ids(&self) -> Vec<BikeShareStationId> {
        self.path.required_station_ids()
    }

    pub fn uses_bike_share(&self) -> bool {
        self.path.uses_bike_share()
    }
}

/// Shared transfer seam for Trip-Based, RAPTOR, CSA, and future algorithms.
pub trait TransferIndex: Send + Sync {
    fn transfers_from(&self, stop: &BoardingPointId) -> &[TransferShortcut];
}

/// Runtime availability is deliberately separate from transit departure
/// timing. Implementations may use a request-local or short-TTL provider
/// cache; the algorithm only receives normalized station facts.
#[async_trait]
pub trait BikeAvailabilityResolver: Send + Sync {
    async fn availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<HashMap<BikeShareStationId, BikeAvailability>, TransferError>;
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransferError {
    #[error("bike-share availability is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("bike-share availability response is invalid")]
    InvalidResponse,
}

/// The in-memory shortcut index used by the production planner.
///
/// The builder intentionally accepts multiple alternatives for a pair of
/// stops. It is compatible with a future ULTRA-style offline preprocessor but
/// does not claim to implement the paper's exact preprocessing algorithm.
#[derive(Clone, Debug, Default)]
pub struct TransferShortcutIndex {
    by_from: HashMap<BoardingPointId, Vec<TransferShortcut>>,
}

impl TransferShortcutIndex {
    pub fn new(shortcuts: impl IntoIterator<Item = TransferShortcut>) -> Self {
        let mut index = Self::default();
        for shortcut in shortcuts {
            index.insert(shortcut);
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
        let alternatives = self.by_from.entry(shortcut.from.clone()).or_default();
        if !alternatives.contains(&shortcut) {
            alternatives.push(shortcut);
        }
    }

    pub fn extend(&mut self, shortcuts: impl IntoIterator<Item = TransferShortcut>) {
        for shortcut in shortcuts {
            self.insert(shortcut);
        }
        self.sort();
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
                    .then(left.walking_seconds.cmp(&right.walking_seconds))
                    .then(left.bike_seconds.cmp(&right.bike_seconds))
            });
        }
    }
}

impl TransferIndex for TransferShortcutIndex {
    fn transfers_from(&self, stop: &BoardingPointId) -> &[TransferShortcut] {
        self.by_from.get(stop).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Filter a shortcut's bike legs against normalized runtime availability.
pub fn shortcut_is_usable(
    shortcut: &TransferShortcut,
    availability: &HashMap<BikeShareStationId, BikeAvailability>,
) -> bool {
    if !shortcut.uses_bike_share() {
        return true;
    }
    shortcut.path.legs.iter().all(|leg| {
        let TransferLeg::BikeShare {
            pickup_station_id,
            return_station_id,
            ..
        } = leg
        else {
            return true;
        };
        let Some(pickup) = availability.get(pickup_station_id) else {
            return false;
        };
        let Some(return_station) = availability.get(return_station_id) else {
            return false;
        };
        pickup.rental_state == crate::domain::realtime::ActionState::Enabled
            && pickup.available_bikes.is_some_and(|count| count > 0)
            && return_station.return_state == crate::domain::realtime::ActionState::Enabled
            && return_station
                .available_return_slots
                .is_some_and(|count| count > 0)
    })
}
