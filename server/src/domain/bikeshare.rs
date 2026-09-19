//! Immutable, provider-neutral static bike-share data.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    ids::{BikeStationId, BoardingPointId},
    location::RequestCoordinate,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BikeStation {
    pub id: BikeStationId,
    pub name: String,
    pub coordinate: RequestCoordinate,
}

impl BikeStation {
    pub fn new(id: BikeStationId, name: impl Into<String>, coordinate: RequestCoordinate) -> Self {
        Self {
            id,
            name: name.into(),
            coordinate,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BikeWalkingAccess {
    pub duration_seconds: u32,
    pub distance_metres: u32,
}

impl BikeWalkingAccess {
    pub fn new(duration_seconds: u32, distance_metres: u32) -> Self {
        Self {
            duration_seconds,
            distance_metres,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BikeStationAccess {
    pub boarding_point_id: BoardingPointId,
    pub station_id: BikeStationId,
    #[serde(default)]
    pub walk_to_station: Option<BikeWalkingAccess>,
    #[serde(default)]
    pub walk_from_station: Option<BikeWalkingAccess>,
}

impl BikeStationAccess {
    pub fn new(
        boarding_point_id: BoardingPointId,
        station_id: BikeStationId,
        walk_to_station: Option<BikeWalkingAccess>,
        walk_from_station: Option<BikeWalkingAccess>,
    ) -> Self {
        Self {
            boarding_point_id,
            station_id,
            walk_to_station,
            walk_from_station,
        }
    }

    pub fn has_direction(&self) -> bool {
        self.walk_to_station.is_some() || self.walk_from_station.is_some()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BikeShareNetwork {
    pub stations: Vec<BikeStation>,
    #[serde(default)]
    pub access_links: Vec<BikeStationAccess>,
    pub static_network_revision: String,
    #[serde(skip, default)]
    indexes: Arc<OnceLock<BikeShareIndexes>>,
}

impl PartialEq for BikeShareNetwork {
    fn eq(&self, other: &Self) -> bool {
        self.stations == other.stations
            && self.access_links == other.access_links
            && self.static_network_revision == other.static_network_revision
    }
}

impl BikeShareNetwork {
    pub fn new(stations: Vec<BikeStation>) -> Self {
        Self {
            stations,
            access_links: Vec::new(),
            static_network_revision: String::new(),
            indexes: Arc::new(OnceLock::new()),
        }
    }

    pub fn with_access_links(mut self, access_links: Vec<BikeStationAccess>) -> Self {
        self.access_links = access_links;
        self.canonicalize()
    }

    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.static_network_revision = revision.into();
        self
    }

    pub fn station(&self, id: &BikeStationId) -> Option<&BikeStation> {
        self.indexes()
            .station_by_id
            .get(id)
            .map(|index| &self.stations[*index])
    }

    pub fn access_from_boarding_point(&self, id: &BoardingPointId) -> &[BikeStationAccess] {
        self.indexes()
            .access_by_boarding_point
            .get(id)
            .map_or(&[], |indices| {
                // The indices are all into the immutable canonical access vector.
                // A contiguous vector is returned by the canonicalizer, so this
                // lookup remains allocation-free for the common case.
                let Some(first) = indices.first() else {
                    return &[];
                };
                let Some(last) = indices.last() else {
                    return &[];
                };
                &self.access_links[*first..=*last]
            })
    }

    pub fn access_to_boarding_point(&self, id: &BoardingPointId) -> Vec<&BikeStationAccess> {
        self.indexes()
            .access_to_boarding_point
            .get(id)
            .map(|indices| {
                indices
                    .iter()
                    .map(|index| &self.access_links[*index])
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), BikeShareNetworkValidationError> {
        if self.static_network_revision.trim().is_empty() {
            return Err(BikeShareNetworkValidationError::MissingRevision);
        }
        let mut station_ids = std::collections::HashSet::new();
        for station in &self.stations {
            if station.id.as_str().trim().is_empty() {
                return Err(BikeShareNetworkValidationError::EmptyStationId);
            }
            if !station_ids.insert(station.id.clone()) {
                return Err(BikeShareNetworkValidationError::DuplicateStation(
                    station.id.clone(),
                ));
            }
            if station.name.trim().is_empty() || !station.coordinate.validate() {
                return Err(BikeShareNetworkValidationError::InvalidStation(
                    station.id.clone(),
                ));
            }
        }
        let mut access_pairs = std::collections::HashSet::new();
        for link in &self.access_links {
            if !station_ids.contains(&link.station_id) {
                return Err(BikeShareNetworkValidationError::UnknownStation(
                    link.station_id.clone(),
                ));
            }
            if !link.has_direction() {
                return Err(BikeShareNetworkValidationError::NoUsableDirection {
                    boarding_point_id: link.boarding_point_id.clone(),
                    station_id: link.station_id.clone(),
                });
            }
            if link
                .walk_to_station
                .is_some_and(|access| access.duration_seconds == 0)
                || link
                    .walk_from_station
                    .is_some_and(|access| access.duration_seconds == 0)
            {
                return Err(BikeShareNetworkValidationError::ZeroDuration {
                    boarding_point_id: link.boarding_point_id.clone(),
                    station_id: link.station_id.clone(),
                });
            }
            if !access_pairs.insert((link.boarding_point_id.clone(), link.station_id.clone())) {
                return Err(BikeShareNetworkValidationError::DuplicateAccessLink {
                    boarding_point_id: link.boarding_point_id.clone(),
                    station_id: link.station_id.clone(),
                });
            }
        }
        Ok(())
    }

    pub fn canonicalize(mut self) -> Self {
        self.stations.sort_by(|left, right| {
            left.id
                .as_str()
                .cmp(right.id.as_str())
                .then(left.name.cmp(&right.name))
                .then(
                    left.coordinate
                        .latitude
                        .total_cmp(&right.coordinate.latitude),
                )
                .then(
                    left.coordinate
                        .longitude
                        .total_cmp(&right.coordinate.longitude),
                )
        });
        self.stations.dedup_by(|left, right| left.id == right.id);

        self.access_links.sort_by(|left, right| {
            left.boarding_point_id
                .as_str()
                .cmp(right.boarding_point_id.as_str())
                .then(left.station_id.as_str().cmp(right.station_id.as_str()))
        });
        let mut merged: Vec<BikeStationAccess> = Vec::with_capacity(self.access_links.len());
        for link in self.access_links.drain(..) {
            if let Some(existing) = merged.last_mut()
                && existing.boarding_point_id == link.boarding_point_id
                && existing.station_id == link.station_id
            {
                existing.walk_to_station =
                    better_access(existing.walk_to_station, link.walk_to_station);
                existing.walk_from_station =
                    better_access(existing.walk_from_station, link.walk_from_station);
            } else {
                merged.push(link);
            }
        }
        self.access_links = merged;
        self.indexes = Arc::new(OnceLock::new());
        self
    }

    pub fn from_artifact_json(input: &str) -> Result<Self, BikeShareArtifactError> {
        let network = serde_json::from_str::<Self>(input)
            .map_err(|error| BikeShareArtifactError::InvalidJson(error.to_string()))?
            .canonicalize();
        network
            .validate()
            .map_err(BikeShareArtifactError::Invalid)
            .map(|_| network)
    }

    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.clone().canonicalize())
    }

    fn indexes(&self) -> &BikeShareIndexes {
        self.indexes.get_or_init(|| BikeShareIndexes::build(self))
    }
}

fn better_access(
    left: Option<BikeWalkingAccess>,
    right: Option<BikeWalkingAccess>,
) -> Option<BikeWalkingAccess> {
    match (left, right) {
        (None, right) => right,
        (left, None) => left,
        (Some(left), Some(right)) => Some(
            if (right.duration_seconds, right.distance_metres)
                < (left.duration_seconds, left.distance_metres)
            {
                right
            } else {
                left
            },
        ),
    }
}

#[derive(Clone, Debug, Default)]
struct BikeShareIndexes {
    station_by_id: HashMap<BikeStationId, usize>,
    access_by_boarding_point: HashMap<BoardingPointId, Vec<usize>>,
    access_to_boarding_point: HashMap<BoardingPointId, Vec<usize>>,
}

impl BikeShareIndexes {
    fn build(network: &BikeShareNetwork) -> Self {
        let mut indexes = Self::default();
        for (index, station) in network.stations.iter().enumerate() {
            indexes.station_by_id.insert(station.id.clone(), index);
        }
        for (index, link) in network.access_links.iter().enumerate() {
            indexes
                .access_by_boarding_point
                .entry(link.boarding_point_id.clone())
                .or_default()
                .push(index);
            if link.walk_from_station.is_some() {
                indexes
                    .access_to_boarding_point
                    .entry(link.boarding_point_id.clone())
                    .or_default()
                    .push(index);
            }
        }
        indexes
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BikeShareNetworkValidationError {
    #[error("bike-share artifact is missing its static network revision")]
    MissingRevision,
    #[error("bike station ID is empty")]
    EmptyStationId,
    #[error("bike station {0:?} is duplicated")]
    DuplicateStation(BikeStationId),
    #[error("bike station {0:?} is malformed")]
    InvalidStation(BikeStationId),
    #[error("bike access references unknown station {0:?}")]
    UnknownStation(BikeStationId),
    #[error("bike access {boarding_point_id:?} ↔ {station_id:?} has no usable direction")]
    NoUsableDirection {
        boarding_point_id: BoardingPointId,
        station_id: BikeStationId,
    },
    #[error("bike access {boarding_point_id:?} ↔ {station_id:?} has zero walking duration")]
    ZeroDuration {
        boarding_point_id: BoardingPointId,
        station_id: BikeStationId,
    },
    #[error("bike access {boarding_point_id:?} ↔ {station_id:?} is duplicated")]
    DuplicateAccessLink {
        boarding_point_id: BoardingPointId,
        station_id: BikeStationId,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BikeShareArtifactError {
    #[error("bike-share artifact JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("bike-share artifact is invalid: {0}")]
    Invalid(BikeShareNetworkValidationError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinate(latitude: f64, longitude: f64) -> RequestCoordinate {
        RequestCoordinate {
            latitude,
            longitude,
        }
    }

    #[test]
    fn canonicalization_and_round_trip_preserve_directional_access() {
        let station_a = BikeStation::new(
            BikeStationId::new("tdx:Taipei:TPE-2"),
            "B",
            coordinate(25.001, 121.501),
        );
        let station_b = BikeStation::new(
            BikeStationId::new("tdx:NewTaipei:NWT-1"),
            "A",
            coordinate(25.0, 121.5),
        );
        let network = BikeShareNetwork::new(vec![station_a, station_b])
            .with_access_links(vec![
                BikeStationAccess::new(
                    BoardingPointId::new("bp-2"),
                    BikeStationId::new("tdx:Taipei:TPE-2"),
                    None,
                    Some(BikeWalkingAccess::new(30, 80)),
                ),
                BikeStationAccess::new(
                    BoardingPointId::new("bp-1"),
                    BikeStationId::new("tdx:NewTaipei:NWT-1"),
                    Some(BikeWalkingAccess::new(20, 50)),
                    None,
                ),
            ])
            .with_revision("sha256:test")
            .canonicalize();

        assert_eq!(network.stations[0].id.as_str(), "tdx:NewTaipei:NWT-1");
        assert_eq!(
            network.access_from_boarding_point(&BoardingPointId::new("bp-2"))[0].walk_from_station,
            Some(BikeWalkingAccess::new(30, 80))
        );
        assert_eq!(
            network
                .access_to_boarding_point(&BoardingPointId::new("bp-2"))
                .len(),
            1
        );
        network.validate().unwrap();

        let json = serde_json::to_string_pretty(&network).unwrap();
        let restored = BikeShareNetwork::from_artifact_json(&json).unwrap();
        assert_eq!(restored, network);
    }

    #[test]
    fn canonicalization_merges_duplicate_directional_links_deterministically() {
        let station = BikeStation::new(
            BikeStationId::new("tdx:Taipei:TPE-1"),
            "Station",
            coordinate(25.0, 121.5),
        );
        let network = BikeShareNetwork::new(vec![station])
            .with_access_links(vec![
                BikeStationAccess::new(
                    BoardingPointId::new("bp"),
                    BikeStationId::new("tdx:Taipei:TPE-1"),
                    Some(BikeWalkingAccess::new(40, 100)),
                    None,
                ),
                BikeStationAccess::new(
                    BoardingPointId::new("bp"),
                    BikeStationId::new("tdx:Taipei:TPE-1"),
                    Some(BikeWalkingAccess::new(30, 120)),
                    Some(BikeWalkingAccess::new(35, 90)),
                ),
            ])
            .with_revision("revision")
            .canonicalize();

        assert_eq!(network.access_links.len(), 1);
        assert_eq!(
            network.access_links[0].walk_to_station,
            Some(BikeWalkingAccess::new(30, 120))
        );
        assert_eq!(
            network.access_links[0].walk_from_station,
            Some(BikeWalkingAccess::new(35, 90))
        );
    }
}
