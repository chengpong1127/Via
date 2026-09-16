//! Algorithm-neutral transfer shortcuts.
//!
//! A shortcut is the only representation a transit algorithm needs for
//! moving between boarding points without boarding another transit trip. The
//! path keeps enough information for reconstruction, while runtime bike
//! availability remains an optional query-time constraint.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::{
    ids::{BikeShareStationId, BoardingPointId},
    realtime::BikeAvailability,
    transit::TransitNetwork,
};

use super::{access::street_metric, service::RoutingLimits};
use crate::street::{
    Coordinate, MatrixOrientation, MatrixQuery, RoutingConstraints, StreetRouter,
    StreetRoutingError, StreetTravelMode,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        self.insert_and_report(shortcut);
    }

    fn insert_and_report(&mut self, shortcut: TransferShortcut) -> bool {
        let alternatives = self.by_from.entry(shortcut.from.clone()).or_default();
        if !alternatives.contains(&shortcut) {
            alternatives.push(shortcut);
            true
        } else {
            false
        }
    }

    pub fn extend(&mut self, shortcuts: impl IntoIterator<Item = TransferShortcut>) {
        self.extend_and_count(shortcuts);
    }

    fn extend_and_count(&mut self, shortcuts: impl IntoIterator<Item = TransferShortcut>) -> usize {
        let mut inserted = 0;
        for shortcut in shortcuts {
            inserted += usize::from(self.insert_and_report(shortcut));
        }
        self.sort();
        inserted
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
                    }
                    | TransferLeg::BikeShare {
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
            for station_id in shortcut.required_station_ids() {
                if network.bike_share_station(&station_id).is_none() {
                    return Err(format!(
                        "transfer shortcut references unknown bike station {}",
                        station_id.as_str()
                    ));
                }
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
                    .then(left.bike_seconds.cmp(&right.bike_seconds))
            });
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferShortcutArtifact {
    pub schema_version: u32,
    pub shortcuts: Vec<TransferShortcut>,
}

/// Builds the request-independent transfer representation used by routing.
///
/// This bounded street-backed builder is intended for explicit offline
/// preprocessing and remains available as a development fallback when no
/// transfer artifact has been supplied. It does not claim exact ULTRA
/// preprocessing. Its output is intentionally the same shortcut/path shape
/// so an offline artifact can replace it without changing Trip-Based or
/// reconstruction code.
pub struct TransferShortcutBuilder {
    street: std::sync::Arc<dyn StreetRouter>,
    network: std::sync::Arc<TransitNetwork>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferPreprocessingProgress {
    pub completed_origin_stops: usize,
    pub total_origin_stops: usize,
    pub transfer_shortcuts: usize,
}

impl TransferShortcutBuilder {
    pub fn new(
        street: std::sync::Arc<dyn StreetRouter>,
        network: std::sync::Arc<TransitNetwork>,
    ) -> Self {
        Self { street, network }
    }

    pub async fn build(
        &self,
        limits: &RoutingLimits,
    ) -> Result<TransferShortcutIndex, StreetRoutingError> {
        self.build_with_progress(limits, &mut |_| {}).await
    }

    pub async fn build_with_progress(
        &self,
        limits: &RoutingLimits,
        progress: &mut dyn FnMut(TransferPreprocessingProgress),
    ) -> Result<TransferShortcutIndex, StreetRoutingError> {
        let mut index = TransferShortcutIndex::from_network(&self.network);
        let stops = self
            .network
            .stops
            .iter()
            .filter_map(|stop| {
                Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)
                    .ok()
                    .map(|coordinate| (stop.id.clone(), coordinate))
            })
            .collect::<Vec<_>>();
        let total_origin_stops = stops.len();
        let mut transfer_shortcuts = index.len();
        progress(TransferPreprocessingProgress {
            completed_origin_stops: 0,
            total_origin_stops,
            transfer_shortcuts,
        });
        for (completed_origin_stops, (from_id, from)) in stops.iter().enumerate() {
            let mut alternatives = Vec::new();
            for (to_id, to) in &stops {
                if from_id == to_id
                    || geographic_distance_metres(*from, *to)
                        > u64::from(limits.max_bike_search_radius_metres)
                {
                    continue;
                }
                alternatives.extend(
                    self.build_bike_shortcuts(*from, *to, from_id.clone(), to_id.clone(), limits)
                        .await?,
                );
            }
            alternatives.sort_by(|left, right| {
                left.duration_seconds
                    .cmp(&right.duration_seconds)
                    .then(left.signature().cmp(&right.signature()))
            });
            alternatives.truncate(limits.max_bike_connectors.max(1));
            transfer_shortcuts += index.extend_and_count(alternatives);
            progress(TransferPreprocessingProgress {
                completed_origin_stops: completed_origin_stops + 1,
                total_origin_stops,
                transfer_shortcuts,
            });
        }
        Ok(index)
    }

    async fn build_bike_shortcuts(
        &self,
        from: Coordinate,
        to: Coordinate,
        from_id: BoardingPointId,
        to_id: BoardingPointId,
        limits: &RoutingLimits,
    ) -> Result<Vec<TransferShortcut>, StreetRoutingError> {
        let pickups = nearby_stations(
            &self.network,
            from,
            limits.max_bike_pickup_stations,
            limits.max_bike_search_radius_metres,
        );
        let returns = nearby_stations(
            &self.network,
            to,
            limits.max_bike_return_stations,
            limits.max_bike_search_radius_metres,
        );
        let mut shortcuts = Vec::new();
        for pickup in pickups {
            let Some(access) = self
                .street
                .matrix(MatrixQuery {
                    orientation: MatrixOrientation::OneToMany,
                    travel_mode: StreetTravelMode::Walking,
                    sources: vec![from],
                    targets: vec![coordinate(pickup.coordinate)?],
                    constraints: RoutingConstraints::default(),
                })
                .await?
                .cells
                .first()
                .and_then(|cells| cells.first())
                .and_then(street_metric)
            else {
                continue;
            };
            for return_station in &returns {
                if pickup.id == return_station.id {
                    continue;
                }
                let Some(bicycle) = self
                    .street
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::OneToMany,
                        travel_mode: StreetTravelMode::Bicycle,
                        sources: vec![coordinate(pickup.coordinate)?],
                        targets: vec![coordinate(return_station.coordinate)?],
                        constraints: RoutingConstraints {
                            max_distance: Some(crate::street::Metres(
                                limits.max_bike_distance_metres,
                            )),
                            ..RoutingConstraints::default()
                        },
                    })
                    .await?
                    .cells
                    .first()
                    .and_then(|cells| cells.first())
                    .and_then(street_metric)
                else {
                    continue;
                };
                if bicycle.duration_seconds > limits.max_bike_duration_seconds
                    || bicycle.distance_metres > limits.max_bike_distance_metres
                {
                    continue;
                }
                let Some(egress) = self
                    .street
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::OneToMany,
                        travel_mode: StreetTravelMode::Walking,
                        sources: vec![coordinate(return_station.coordinate)?],
                        targets: vec![to],
                        constraints: RoutingConstraints::default(),
                    })
                    .await?
                    .cells
                    .first()
                    .and_then(|cells| cells.first())
                    .and_then(street_metric)
                else {
                    continue;
                };
                shortcuts.push(TransferShortcut::bike_share(
                    from_id.clone(),
                    to_id.clone(),
                    TransferPath::walking_and_bike(
                        access.duration_seconds,
                        access.distance_metres,
                        pickup.id.clone(),
                        return_station.id.clone(),
                        bicycle.duration_seconds,
                        bicycle.distance_metres,
                        egress.duration_seconds,
                        egress.distance_metres,
                    ),
                ));
            }
        }
        Ok(shortcuts)
    }
}

impl TransferShortcut {
    fn signature(&self) -> String {
        format!(
            "{}:{}:{}",
            self.from.as_str(),
            self.to.as_str(),
            self.required_station_ids()
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

fn nearby_stations(
    network: &TransitNetwork,
    coordinate: Coordinate,
    limit: usize,
    radius_metres: u32,
) -> Vec<&crate::domain::transit::BikeShareStation> {
    let mut stations = network
        .bike_share_stations
        .iter()
        .filter(|station| {
            geographic_distance_metres(
                coordinate,
                Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude)
                    .expect("validated station coordinate"),
            ) <= u64::from(radius_metres)
        })
        .collect::<Vec<_>>();
    stations.sort_by(|left, right| {
        geographic_distance_metres(
            coordinate,
            Coordinate::try_new(left.coordinate.latitude, left.coordinate.longitude)
                .expect("validated station coordinate"),
        )
        .cmp(&geographic_distance_metres(
            coordinate,
            Coordinate::try_new(right.coordinate.latitude, right.coordinate.longitude)
                .expect("validated station coordinate"),
        ))
        .then(left.id.as_str().cmp(right.id.as_str()))
    });
    stations.truncate(limit.max(1));
    stations
}

fn coordinate(value: crate::api::RequestCoordinate) -> Result<Coordinate, StreetRoutingError> {
    Coordinate::try_new(value.latitude, value.longitude)
}

fn geographic_distance_metres(from: Coordinate, to: Coordinate) -> u64 {
    let latitude = ((from.latitude() + to.latitude()) / 2.0).to_radians();
    let north = (to.latitude() - from.latitude()) * 111_320.0;
    let east = (to.longitude() - from.longitude()) * 111_320.0 * latitude.cos();
    north.hypot(east).max(0.0).round() as u64
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::Time;

    use super::*;
    use crate::{
        domain::{
            ids::BoardingPointId,
            location::RequestCoordinate,
            transit::{TransitNetwork, TransitStop},
        },
        street::StraightLineStreetRouter,
    };

    #[tokio::test]
    async fn reports_progress_for_completed_origin_stops() {
        let network = Arc::new(TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![
                TransitStop::new(
                    BoardingPointId::new("a"),
                    "A",
                    RequestCoordinate {
                        latitude: 25.04,
                        longitude: 121.56,
                    },
                ),
                TransitStop::new(
                    BoardingPointId::new("b"),
                    "B",
                    RequestCoordinate {
                        latitude: 25.041,
                        longitude: 121.561,
                    },
                ),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
        let builder =
            TransferShortcutBuilder::new(Arc::new(StraightLineStreetRouter::default()), network);
        let mut reports = Vec::new();

        builder
            .build_with_progress(&RoutingLimits::default(), &mut |progress| {
                reports.push(progress)
            })
            .await
            .unwrap();

        assert_eq!(
            reports,
            vec![
                TransferPreprocessingProgress {
                    completed_origin_stops: 0,
                    total_origin_stops: 2,
                    transfer_shortcuts: 0,
                },
                TransferPreprocessingProgress {
                    completed_origin_stops: 1,
                    total_origin_stops: 2,
                    transfer_shortcuts: 0,
                },
                TransferPreprocessingProgress {
                    completed_origin_stops: 2,
                    total_origin_stops: 2,
                    transfer_shortcuts: 0,
                },
            ]
        );
    }
}
