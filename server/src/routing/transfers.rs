//! Algorithm-neutral transfer shortcuts.
//!
//! A shortcut is the only representation a transit algorithm needs for
//! moving between boarding points without boarding another transit trip. The
//! path keeps enough information for reconstruction, while runtime bike
//! availability remains an optional query-time constraint.

use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    pub current_candidates: usize,
    pub pickups: usize,
    pub returns: usize,
    pub street_matrix_batches: usize,
}

#[derive(Clone, Debug)]
struct StopCandidate {
    id: BoardingPointId,
    coordinate: Coordinate,
    station_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum MatrixEndpoint {
    Stop(usize),
    Station(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct MatrixMetricKey {
    source: MatrixEndpoint,
    target: MatrixEndpoint,
}

#[derive(Clone, Copy, Debug)]
struct MatrixPair {
    key: MatrixMetricKey,
    source: Coordinate,
    target: Coordinate,
}

struct MatrixBatch {
    sources: Vec<(MatrixEndpoint, Coordinate)>,
    targets: Vec<(MatrixEndpoint, Coordinate)>,
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
        // Keep explicit static transit/walking edges in the index, but generate
        // only street-backed YouBike alternatives in this preprocessing pass.
        let mut index = TransferShortcutIndex::from_network(&self.network);
        let stops = self
            .network
            .stops
            .iter()
            .filter_map(|stop| {
                Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)
                    .ok()
                    .map(|coordinate| StopCandidate {
                        id: stop.id.clone(),
                        coordinate,
                        station_indices: Vec::new(),
                    })
            })
            .collect::<Vec<_>>();
        let stop_indices = stops
            .iter()
            .enumerate()
            .map(|(index, stop)| (stop.id.clone(), index))
            .collect::<HashMap<_, _>>();
        let station_indices = self
            .network
            .bike_share_stations
            .iter()
            .enumerate()
            .map(|(index, station)| (station.id.clone(), index))
            .collect::<HashMap<_, _>>();
        let station_candidate_limit = limits
            .max_bike_pickup_stations
            .max(limits.max_bike_return_stations)
            .max(1);
        let station_candidates = stops
            .iter()
            .map(|stop| {
                self.network
                    .bike_share_stations_within_radius(
                        request_coordinate(stop.coordinate),
                        limits.max_bike_search_radius_metres,
                    )
                    .into_iter()
                    .take(station_candidate_limit)
                    .filter_map(|station| station_indices.get(&station.id).copied())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut stops = stops;
        for (stop, station_indices) in stops.iter_mut().zip(station_candidates) {
            stop.station_indices = station_indices;
        }
        let total_origin_stops = stops.len();
        let mut transfer_shortcuts = index.len();
        let mut report = |completed_origin_stops,
                          current_candidates,
                          pickups,
                          returns,
                          street_matrix_batches,
                          transfer_shortcuts| {
            progress(TransferPreprocessingProgress {
                completed_origin_stops,
                total_origin_stops,
                transfer_shortcuts,
                current_candidates,
                pickups,
                returns,
                street_matrix_batches,
            });
        };
        report(0, 0, 0, 0, 0, transfer_shortcuts);
        let mut walking_cache = HashMap::new();
        let mut bicycle_cache = HashMap::new();
        for (completed_origin_stops, origin) in stops.iter().enumerate() {
            let destination_indices = self
                .network
                .stops_within_radius(
                    request_coordinate(origin.coordinate),
                    limits.max_bike_search_radius_metres,
                )
                .into_iter()
                .filter_map(|stop| stop_indices.get(&stop.id).copied())
                .filter(|index| *index != completed_origin_stops)
                .collect::<Vec<_>>();
            let current_candidates = destination_indices.len();
            let pickup_indices = origin
                .station_indices
                .iter()
                .copied()
                .take(limits.max_bike_pickup_stations.max(1))
                .collect::<Vec<_>>();
            let max_returns = limits.max_bike_return_stations.max(1);
            let return_indices = destination_indices
                .iter()
                .flat_map(|index| {
                    stops[*index]
                        .station_indices
                        .iter()
                        .copied()
                        .take(max_returns)
                })
                .collect::<BTreeSet<_>>();
            let mut street_matrix_batches = 0;
            report(
                completed_origin_stops,
                current_candidates,
                pickup_indices.len(),
                return_indices.len(),
                street_matrix_batches,
                transfer_shortcuts,
            );
            let access_pairs = pickup_indices
                .iter()
                .map(|station_index| {
                    Ok(MatrixPair {
                        key: MatrixMetricKey {
                            source: MatrixEndpoint::Stop(completed_origin_stops),
                            target: MatrixEndpoint::Station(*station_index),
                        },
                        source: origin.coordinate,
                        target: station_coordinate(&self.network, *station_index)?,
                    })
                })
                .collect::<Result<Vec<_>, StreetRoutingError>>()?;
            street_matrix_batches += resolve_matrix_pairs(
                self.street.as_ref(),
                &access_pairs,
                StreetTravelMode::Walking,
                RoutingConstraints::default(),
                &mut walking_cache,
            )
            .await?;
            report(
                completed_origin_stops,
                current_candidates,
                pickup_indices.len(),
                return_indices.len(),
                street_matrix_batches,
                transfer_shortcuts,
            );
            let accessible_pickups = pickup_indices
                .iter()
                .copied()
                .filter(|station_index| {
                    walking_cache
                        .get(&MatrixMetricKey {
                            source: MatrixEndpoint::Stop(completed_origin_stops),
                            target: MatrixEndpoint::Station(*station_index),
                        })
                        .copied()
                        .flatten()
                        .is_some()
                })
                .collect::<Vec<_>>();
            let bicycle_pairs = accessible_pickups
                .iter()
                .flat_map(|pickup_index| {
                    return_indices
                        .iter()
                        .filter(|return_index| *pickup_index != **return_index)
                        .map(|return_index| {
                            Ok(MatrixPair {
                                key: MatrixMetricKey {
                                    source: MatrixEndpoint::Station(*pickup_index),
                                    target: MatrixEndpoint::Station(*return_index),
                                },
                                source: station_coordinate(&self.network, *pickup_index)
                                    .expect("indexed station coordinate is valid"),
                                target: station_coordinate(&self.network, *return_index)
                                    .expect("indexed station coordinate is valid"),
                            })
                        })
                })
                .collect::<Result<Vec<_>, StreetRoutingError>>()?;
            street_matrix_batches += resolve_matrix_pairs(
                self.street.as_ref(),
                &bicycle_pairs,
                StreetTravelMode::Bicycle,
                RoutingConstraints {
                    max_distance: Some(crate::street::Metres(limits.max_bike_distance_metres)),
                    ..RoutingConstraints::default()
                },
                &mut bicycle_cache,
            )
            .await?;
            report(
                completed_origin_stops,
                current_candidates,
                pickup_indices.len(),
                return_indices.len(),
                street_matrix_batches,
                transfer_shortcuts,
            );
            let usable_bicycle = |pickup_index: usize, return_index: usize| {
                bicycle_cache
                    .get(&MatrixMetricKey {
                        source: MatrixEndpoint::Station(pickup_index),
                        target: MatrixEndpoint::Station(return_index),
                    })
                    .copied()
                    .flatten()
                    .filter(|metric| {
                        metric.duration_seconds <= limits.max_bike_duration_seconds
                            && metric.distance_metres <= limits.max_bike_distance_metres
                    })
            };
            let usable_return_indices = accessible_pickups
                .iter()
                .flat_map(|pickup_index| {
                    return_indices.iter().filter_map(|return_index| {
                        usable_bicycle(*pickup_index, *return_index).map(|_| *return_index)
                    })
                })
                .collect::<BTreeSet<_>>();
            let egress_pairs = destination_indices
                .iter()
                .flat_map(|destination_index| {
                    stops[*destination_index]
                        .station_indices
                        .iter()
                        .copied()
                        .take(max_returns)
                        .filter(|return_index| usable_return_indices.contains(return_index))
                        .map(|return_index| {
                            Ok(MatrixPair {
                                key: MatrixMetricKey {
                                    source: MatrixEndpoint::Station(return_index),
                                    target: MatrixEndpoint::Stop(*destination_index),
                                },
                                source: station_coordinate(&self.network, return_index)
                                    .expect("indexed station coordinate is valid"),
                                target: stops[*destination_index].coordinate,
                            })
                        })
                })
                .collect::<Result<Vec<_>, StreetRoutingError>>()?;
            street_matrix_batches += resolve_matrix_pairs(
                self.street.as_ref(),
                &egress_pairs,
                StreetTravelMode::Walking,
                RoutingConstraints::default(),
                &mut walking_cache,
            )
            .await?;
            report(
                completed_origin_stops,
                current_candidates,
                pickup_indices.len(),
                return_indices.len(),
                street_matrix_batches,
                transfer_shortcuts,
            );
            let mut alternatives = Vec::new();
            for destination_index in destination_indices {
                let destination = &stops[destination_index];
                for pickup_index in &accessible_pickups {
                    let Some(access) = walking_cache
                        .get(&MatrixMetricKey {
                            source: MatrixEndpoint::Stop(completed_origin_stops),
                            target: MatrixEndpoint::Station(*pickup_index),
                        })
                        .copied()
                        .flatten()
                    else {
                        continue;
                    };
                    for return_index in destination
                        .station_indices
                        .iter()
                        .copied()
                        .take(max_returns)
                    {
                        if *pickup_index == return_index {
                            continue;
                        }
                        let Some(bicycle) = usable_bicycle(*pickup_index, return_index) else {
                            continue;
                        };
                        let Some(egress) = walking_cache
                            .get(&MatrixMetricKey {
                                source: MatrixEndpoint::Station(return_index),
                                target: MatrixEndpoint::Stop(destination_index),
                            })
                            .copied()
                            .flatten()
                        else {
                            continue;
                        };
                        alternatives.push(TransferShortcut::bike_share(
                            origin.id.clone(),
                            destination.id.clone(),
                            TransferPath::walking_and_bike(
                                access.duration_seconds,
                                access.distance_metres,
                                self.network.bike_share_stations[*pickup_index].id.clone(),
                                self.network.bike_share_stations[return_index].id.clone(),
                                bicycle.duration_seconds,
                                bicycle.distance_metres,
                                egress.duration_seconds,
                                egress.distance_metres,
                            ),
                        ));
                    }
                }
            }
            alternatives.sort_by(|left, right| {
                left.duration_seconds
                    .cmp(&right.duration_seconds)
                    .then(left.signature().cmp(&right.signature()))
            });
            alternatives.dedup();
            alternatives.truncate(limits.max_bike_connectors.max(1));
            transfer_shortcuts += index.extend_and_count(alternatives);
            report(
                completed_origin_stops + 1,
                current_candidates,
                pickup_indices.len(),
                return_indices.len(),
                street_matrix_batches,
                transfer_shortcuts,
            );
        }
        Ok(index)
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

fn coordinate(value: crate::api::RequestCoordinate) -> Result<Coordinate, StreetRoutingError> {
    Coordinate::try_new(value.latitude, value.longitude)
}

fn station_coordinate(
    network: &TransitNetwork,
    index: usize,
) -> Result<Coordinate, StreetRoutingError> {
    network
        .bike_share_stations
        .get(index)
        .ok_or_else(|| StreetRoutingError::InvalidRequest {
            reason: "transfer preprocessing referenced an unknown bike station".into(),
        })
        .and_then(|station| coordinate(station.coordinate))
}

fn request_coordinate(coordinate: Coordinate) -> crate::api::RequestCoordinate {
    crate::api::RequestCoordinate {
        latitude: coordinate.latitude(),
        longitude: coordinate.longitude(),
    }
}

async fn resolve_matrix_pairs(
    street: &dyn StreetRouter,
    pairs: &[MatrixPair],
    travel_mode: StreetTravelMode,
    constraints: RoutingConstraints,
    cache: &mut HashMap<MatrixMetricKey, Option<crate::routing::access::StreetMetric>>,
) -> Result<usize, StreetRoutingError> {
    let matrix_limits = street.matrix_limits();
    let mut pending = BTreeMap::new();
    for pair in pairs {
        if !cache.contains_key(&pair.key) {
            pending.entry(pair.key).or_insert(*pair);
        }
    }
    let mut batches = 0;
    while !pending.is_empty() {
        let batch = take_matrix_batch(&mut pending, matrix_limits);
        let matrix = street
            .matrix(MatrixQuery {
                orientation: matrix_orientation(batch.sources.len(), batch.targets.len()),
                travel_mode,
                sources: batch
                    .sources
                    .iter()
                    .map(|(_, coordinate)| *coordinate)
                    .collect(),
                targets: batch
                    .targets
                    .iter()
                    .map(|(_, coordinate)| *coordinate)
                    .collect(),
                constraints,
            })
            .await?;
        if matrix.cells.len() != batch.sources.len()
            || matrix
                .cells
                .iter()
                .any(|row| row.len() != batch.targets.len())
        {
            return Err(StreetRoutingError::MalformedResponse);
        }
        for (source_index, (source, _)) in batch.sources.iter().enumerate() {
            for (target_index, (target, _)) in batch.targets.iter().enumerate() {
                cache.insert(
                    MatrixMetricKey {
                        source: *source,
                        target: *target,
                    },
                    street_metric(&matrix.cells[source_index][target_index]),
                );
            }
        }
        batches += 1;
    }
    Ok(batches)
}

fn take_matrix_batch(
    pending: &mut BTreeMap<MatrixMetricKey, MatrixPair>,
    matrix_limits: crate::street::MatrixLimits,
) -> MatrixBatch {
    let max_sources = matrix_limits.max_sources.max(1);
    let max_targets = matrix_limits.max_targets.max(1);
    let first_key = *pending.keys().next().expect("matrix batch is not empty");
    let mut sources = vec![first_key.source];
    let mut targets = vec![first_key.target];
    loop {
        let source_count = sources.len();
        let target_count = targets.len();
        let candidate_targets = pending
            .keys()
            .map(|key| key.target)
            .collect::<BTreeSet<_>>();
        for target in candidate_targets {
            if targets.len() >= max_targets
                || targets.contains(&target)
                || !sources.iter().all(|source| {
                    pending.contains_key(&MatrixMetricKey {
                        source: *source,
                        target,
                    })
                })
            {
                continue;
            }
            targets.push(target);
        }
        let candidate_sources = pending
            .keys()
            .map(|key| key.source)
            .collect::<BTreeSet<_>>();
        for source in candidate_sources {
            if sources.len() >= max_sources
                || sources.contains(&source)
                || !targets.iter().all(|target| {
                    pending.contains_key(&MatrixMetricKey {
                        source,
                        target: *target,
                    })
                })
            {
                continue;
            }
            sources.push(source);
        }
        if source_count == sources.len() && target_count == targets.len() {
            break;
        }
    }
    let source_coordinates = sources
        .iter()
        .map(|source| {
            pending
                .get(&MatrixMetricKey {
                    source: *source,
                    target: targets[0],
                })
                .expect("matrix batch contains every source-target pair")
                .source
        })
        .collect::<Vec<_>>();
    let target_coordinates = targets
        .iter()
        .map(|target| {
            pending
                .get(&MatrixMetricKey {
                    source: sources[0],
                    target: *target,
                })
                .expect("matrix batch contains every source-target pair")
                .target
        })
        .collect::<Vec<_>>();
    for source in &sources {
        for target in &targets {
            pending.remove(&MatrixMetricKey {
                source: *source,
                target: *target,
            });
        }
    }
    MatrixBatch {
        sources: sources.into_iter().zip(source_coordinates).collect(),
        targets: targets.into_iter().zip(target_coordinates).collect(),
    }
}

fn matrix_orientation(source_count: usize, target_count: usize) -> MatrixOrientation {
    match (source_count, target_count) {
        (1, _) => MatrixOrientation::OneToMany,
        (_, 1) => MatrixOrientation::ManyToOne,
        _ => MatrixOrientation::ManyToMany,
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
        bike_availability_is_usable(
            availability.get(pickup_station_id),
            availability.get(return_station_id),
        )
    })
}

pub fn bike_availability_is_usable(
    pickup: Option<&BikeAvailability>,
    return_station: Option<&BikeAvailability>,
) -> bool {
    pickup.is_none_or(|availability| {
        availability.rental_state != crate::domain::realtime::ActionState::Disabled
            && availability.available_bikes != Some(0)
    }) && return_station.is_none_or(|availability| {
        availability.return_state != crate::domain::realtime::ActionState::Disabled
            && availability.available_return_slots != Some(0)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use time::Time;

    use super::*;
    use crate::{
        domain::{
            ids::{BikeShareStationId, BoardingPointId},
            location::RequestCoordinate,
            transit::{BikeShareStation, TransitNetwork, TransitStop},
        },
        street::{
            MatrixCell, MatrixLimits, MatrixQuery, MatrixResult, RouteQuery,
            StraightLineStreetRouter, StreetPath, StreetRouter, StreetRoutingError,
            StreetTravelMode,
        },
    };

    fn request_coordinate(latitude: f64, longitude: f64) -> RequestCoordinate {
        RequestCoordinate {
            latitude,
            longitude,
        }
    }

    fn stop(id: &str, latitude: f64, longitude: f64) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(id),
            id,
            request_coordinate(latitude, longitude),
        )
    }

    fn station(id: &str, latitude: f64, longitude: f64) -> BikeShareStation {
        BikeShareStation {
            id: BikeShareStationId::new(id),
            name: id.into(),
            coordinate: request_coordinate(latitude, longitude),
        }
    }

    fn optimization_network(reverse_order: bool) -> Arc<TransitNetwork> {
        let mut stops = vec![
            stop("stop-a", 25.000, 121.500),
            stop("stop-b", 25.002, 121.502),
            stop("stop-c", 25.004, 121.504),
        ];
        let mut stations = vec![
            station("bike-a", 25.001, 121.501),
            station("bike-b", 25.003, 121.503),
            station("bike-c", 25.005, 121.505),
        ];
        if reverse_order {
            stops.reverse();
            stations.reverse();
        }
        Arc::new(TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            stops,
            Vec::new(),
            Vec::new(),
            stations,
        ))
    }

    fn optimization_limits() -> RoutingLimits {
        RoutingLimits {
            max_bike_search_radius_metres: 1_000,
            max_bike_pickup_stations: 2,
            max_bike_return_stations: 2,
            max_bike_connectors: 24,
            ..RoutingLimits::default()
        }
    }

    #[derive(Clone)]
    struct RecordingStreetRouter {
        inner: StraightLineStreetRouter,
        calls: Arc<Mutex<Vec<MatrixQuery>>>,
        unreachable_bicycle: bool,
        matrix_limits: MatrixLimits,
    }

    impl RecordingStreetRouter {
        fn new(unreachable_bicycle: bool) -> Self {
            Self {
                inner: StraightLineStreetRouter::default(),
                calls: Arc::new(Mutex::new(Vec::new())),
                unreachable_bicycle,
                matrix_limits: MatrixLimits::default(),
            }
        }

        fn with_matrix_limits(mut self, matrix_limits: MatrixLimits) -> Self {
            self.matrix_limits = matrix_limits;
            self
        }

        fn calls(&self) -> Vec<MatrixQuery> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl StreetRouter for RecordingStreetRouter {
        fn matrix_limits(&self) -> MatrixLimits {
            self.matrix_limits
        }

        async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
            self.calls.lock().unwrap().push(query.clone());
            if self.unreachable_bicycle && query.travel_mode == StreetTravelMode::Bicycle {
                return Ok(MatrixResult {
                    cells: vec![
                        vec![MatrixCell::Unreachable; query.targets.len()];
                        query.sources.len()
                    ],
                });
            }
            self.inner.matrix(query).await
        }

        async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
            self.inner.route(query).await
        }
    }

    #[test]
    fn spatial_index_excludes_distant_stops_and_orders_nearby_stations() {
        let network = TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            vec![
                stop("far", 25.10, 121.60),
                stop("near", 25.001, 121.501),
                stop("middle", 25.004, 121.504),
            ],
            Vec::new(),
            Vec::new(),
            vec![
                station("far-bike", 25.10, 121.60),
                station("middle-bike", 25.003, 121.503),
                station("near-bike", 25.001, 121.501),
            ],
        );
        let origin = request_coordinate(25.0, 121.5);
        let nearby_stop_ids = network
            .stops_within_radius(origin, 1_000)
            .into_iter()
            .map(|stop| stop.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(nearby_stop_ids, vec!["near", "middle"]);

        let nearby_station_ids = network
            .bike_share_stations_within_radius(origin, 1_000)
            .into_iter()
            .take(2)
            .map(|station| station.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(nearby_station_ids, vec!["near-bike", "middle-bike"]);
    }

    #[tokio::test]
    async fn batched_preprocessing_preserves_shortcuts_and_reuses_matrix_pairs() {
        let network = optimization_network(false);
        let street = RecordingStreetRouter::new(false);
        let builder = TransferShortcutBuilder::new(Arc::new(street.clone()), network);
        let mut reports = Vec::new();
        let index = builder
            .build_with_progress(&optimization_limits(), &mut |progress| {
                reports.push(progress)
            })
            .await
            .unwrap();

        assert_eq!(index.len(), 16);
        assert!(index.shortcuts().windows(2).all(|pair| pair[0] != pair[1]));
        assert!(street.calls().len() < 20);
        assert!(
            street
                .calls()
                .iter()
                .any(|query| query.sources.len() > 1 || query.targets.len() > 1)
        );
        assert_eq!(reports.last().unwrap().completed_origin_stops, 3);
        assert_eq!(reports.last().unwrap().total_origin_stops, 3);
        assert!(reports.last().unwrap().current_candidates > 0);
        assert!(reports.last().unwrap().street_matrix_batches > 0);

        let scalar = TransferShortcutBuilder::new(
            Arc::new(
                RecordingStreetRouter::new(false).with_matrix_limits(MatrixLimits {
                    max_sources: 1,
                    max_targets: 1,
                }),
            ),
            optimization_network(false),
        )
        .build(&optimization_limits())
        .await
        .unwrap();
        assert_eq!(
            index.canonical_json().unwrap(),
            scalar.canonical_json().unwrap()
        );

        let reversed_network = optimization_network(true);
        let reversed = TransferShortcutBuilder::new(
            Arc::new(RecordingStreetRouter::new(false)),
            reversed_network,
        )
        .build(&optimization_limits())
        .await
        .unwrap();
        assert_eq!(
            index.canonical_json().unwrap(),
            reversed.canonical_json().unwrap()
        );
    }

    #[tokio::test]
    async fn unreachable_matrix_cells_are_ignored_without_partial_shortcuts() {
        let network = optimization_network(false);
        let street = RecordingStreetRouter::new(true);
        let index = TransferShortcutBuilder::new(Arc::new(street), network)
            .build(&optimization_limits())
            .await
            .unwrap();

        assert!(index.is_empty());
    }

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
            reports.first().copied(),
            Some(TransferPreprocessingProgress {
                completed_origin_stops: 0,
                total_origin_stops: 2,
                transfer_shortcuts: 0,
                current_candidates: 0,
                pickups: 0,
                returns: 0,
                street_matrix_batches: 0,
            })
        );
        assert!(reports.iter().any(|report| {
            report.completed_origin_stops == 0 && report.current_candidates == 1
        }));
        assert_eq!(
            reports.last().copied(),
            Some(TransferPreprocessingProgress {
                completed_origin_stops: 2,
                total_origin_stops: 2,
                transfer_shortcuts: 0,
                current_candidates: 1,
                pickups: 0,
                returns: 0,
                street_matrix_batches: 0,
            })
        );
    }
}
