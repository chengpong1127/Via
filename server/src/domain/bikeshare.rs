use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use super::{
    ids::{BikeStationId, BoardingPointId},
    location::GeoCoordinate,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BikeStation {
    pub id: BikeStationId,
    pub name: String,
    pub coordinate: GeoCoordinate,
}

impl BikeStation {
    pub fn new(id: BikeStationId, name: impl Into<String>, coordinate: GeoCoordinate) -> Self {
        Self {
            id,
            name: name.into(),
            coordinate,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

    fn has_direction(&self) -> bool {
        self.walk_to_station.is_some() || self.walk_from_station.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BikeStationAvailability {
    pub station_id: BikeStationId,
    pub available_bikes: u32,
    pub available_return_slots: u32,
    pub in_service: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
}

impl BikeStationAvailability {
    pub fn new(
        station_id: BikeStationId,
        available_bikes: u32,
        available_return_slots: u32,
        in_service: bool,
        observed_at: OffsetDateTime,
    ) -> Self {
        Self {
            station_id,
            available_bikes,
            available_return_slots,
            in_service,
            observed_at,
        }
    }

    pub fn is_fresh(&self, now: OffsetDateTime, max_age: Duration) -> bool {
        self.is_fresh_with_skew(now, max_age, MAX_BIKE_AVAILABILITY_CLOCK_SKEW)
    }

    pub fn is_fresh_with_skew(
        &self,
        now: OffsetDateTime,
        max_age: Duration,
        max_clock_skew: Duration,
    ) -> bool {
        let age = now - self.observed_at;
        age >= -max_clock_skew && age <= max_age
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BikeAvailabilitySnapshot {
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    pub stations: HashMap<BikeStationId, BikeStationAvailability>,
}

impl BikeAvailabilitySnapshot {
    pub fn new(
        received_at: OffsetDateTime,
        stations: HashMap<BikeStationId, BikeStationAvailability>,
    ) -> Self {
        Self {
            received_at,
            stations,
        }
    }

    pub fn station(&self, id: &BikeStationId) -> Option<&BikeStationAvailability> {
        self.stations.get(id)
    }
}

pub const DEFAULT_BIKE_AVAILABILITY_MAX_AGE: Duration = Duration::seconds(120);
pub const MAX_BIKE_AVAILABILITY_CLOCK_SKEW: Duration = Duration::seconds(300);

pub struct BikeAvailabilityView<'a> {
    network: &'a BikeShareNetwork,
    snapshot: &'a BikeAvailabilitySnapshot,
    now: OffsetDateTime,
    max_age: Duration,
    max_clock_skew: Duration,
}

impl<'a> BikeAvailabilityView<'a> {
    fn new(network: &'a BikeShareNetwork, snapshot: &'a BikeAvailabilitySnapshot) -> Self {
        Self::at(
            network,
            snapshot,
            OffsetDateTime::now_utc(),
            DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
        )
    }

    fn at(
        network: &'a BikeShareNetwork,
        snapshot: &'a BikeAvailabilitySnapshot,
        now: OffsetDateTime,
        max_age: Duration,
    ) -> Self {
        Self {
            network,
            snapshot,
            now,
            max_age,
            max_clock_skew: MAX_BIKE_AVAILABILITY_CLOCK_SKEW,
        }
    }

    pub fn usable_pickups(
        &self,
        boarding_point_id: &BoardingPointId,
    ) -> Vec<&'a BikeStationAccess> {
        self.network
            .access_from_boarding_point(boarding_point_id)
            .iter()
            .filter(|access| access.walk_to_station.is_some())
            .filter(|access| self.station_usable_as_pickup(&access.station_id))
            .collect()
    }

    pub fn usable_dropoffs(
        &self,
        boarding_point_id: &BoardingPointId,
    ) -> Vec<&'a BikeStationAccess> {
        self.network
            .access_to_boarding_point(boarding_point_id)
            .into_iter()
            .filter(|access| access.walk_from_station.is_some())
            .filter(|access| self.station_usable_as_dropoff(&access.station_id))
            .collect()
    }

    pub fn can_pick_up_at(&self, station_id: &BikeStationId) -> bool {
        self.station_usable_as_pickup(station_id)
    }

    pub fn can_return_at(&self, station_id: &BikeStationId) -> bool {
        self.station_usable_as_dropoff(station_id)
    }

    fn station_usable_as_pickup(&self, station_id: &BikeStationId) -> bool {
        self.station_is_serviceable(station_id)
            && self
                .snapshot
                .station(station_id)
                .is_some_and(|availability| availability.available_bikes > 0)
    }

    fn station_usable_as_dropoff(&self, station_id: &BikeStationId) -> bool {
        self.station_is_serviceable(station_id)
            && self
                .snapshot
                .station(station_id)
                .is_some_and(|availability| availability.available_return_slots > 0)
    }

    fn station_is_serviceable(&self, station_id: &BikeStationId) -> bool {
        self.network.station(station_id).is_some_and(|_| {
            self.snapshot
                .station(station_id)
                .is_some_and(|availability| {
                    availability.is_fresh_with_skew(self.now, self.max_age, self.max_clock_skew)
                        && availability.in_service
                })
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    pub(crate) fn stations_within_radius(
        &self,
        point: GeoCoordinate,
        radius_metres: f64,
    ) -> Vec<&BikeStation> {
        self.indexes()
            .station_positions
            .within_radius(&self.stations, point, radius_metres)
            .into_iter()
            .map(|index| &self.stations[index])
            .collect()
    }

    pub fn station(&self, id: &BikeStationId) -> Option<&BikeStation> {
        self.indexes()
            .station_by_id
            .get(id)
            .map(|index| &self.stations[*index])
    }

    pub(crate) fn required_station(&self, id: &BikeStationId) -> &BikeStation {
        self.station(id).expect("validated bike station id")
    }

    pub fn access_from_boarding_point(&self, id: &BoardingPointId) -> &[BikeStationAccess] {
        self.indexes()
            .access_by_boarding_point
            .get(id)
            .map_or(&[], |indices| {
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

    pub fn availability_view<'a>(
        &'a self,
        snapshot: &'a BikeAvailabilitySnapshot,
    ) -> BikeAvailabilityView<'a> {
        BikeAvailabilityView::new(self, snapshot)
    }

    pub fn availability_view_at<'a>(
        &'a self,
        snapshot: &'a BikeAvailabilitySnapshot,
        now: OffsetDateTime,
        max_age: Duration,
    ) -> BikeAvailabilityView<'a> {
        BikeAvailabilityView::at(self, snapshot, now, max_age)
    }

    pub fn availability_view_with_policy<'a>(
        &'a self,
        snapshot: &'a BikeAvailabilitySnapshot,
        now: OffsetDateTime,
        max_age: Duration,
        max_clock_skew: Duration,
    ) -> BikeAvailabilityView<'a> {
        let mut view = BikeAvailabilityView::at(self, snapshot, now, max_age);
        view.max_clock_skew = max_clock_skew;
        view
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

    pub(crate) fn canonicalize(mut self) -> Self {
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
    station_positions: super::spatial::SpatialPointIndex,
    station_by_id: HashMap<BikeStationId, usize>,
    access_by_boarding_point: HashMap<BoardingPointId, Vec<usize>>,
    access_to_boarding_point: HashMap<BoardingPointId, Vec<usize>>,
}

impl BikeShareIndexes {
    fn build(network: &BikeShareNetwork) -> Self {
        let mut indexes = Self {
            station_positions: super::spatial::SpatialPointIndex::build(&network.stations),
            ..Self::default()
        };
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
    use std::collections::HashMap;

    use super::*;

    fn coordinate(latitude: f64, longitude: f64) -> GeoCoordinate {
        GeoCoordinate {
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

    #[test]
    fn availability_view_filters_directional_access_and_fresh_serviceable_counts() {
        let station_a = BikeStation::new(
            BikeStationId::new("tdx:Taipei:A"),
            "A",
            coordinate(25.0, 121.5),
        );
        let station_b = BikeStation::new(
            BikeStationId::new("tdx:Taipei:B"),
            "B",
            coordinate(25.001, 121.501),
        );
        let network = BikeShareNetwork::new(vec![station_a, station_b])
            .with_access_links(vec![
                BikeStationAccess::new(
                    BoardingPointId::new("bp"),
                    BikeStationId::new("tdx:Taipei:A"),
                    Some(BikeWalkingAccess::new(10, 50)),
                    None,
                ),
                BikeStationAccess::new(
                    BoardingPointId::new("bp"),
                    BikeStationId::new("tdx:Taipei:B"),
                    None,
                    Some(BikeWalkingAccess::new(20, 80)),
                ),
            ])
            .with_revision("revision");
        let now = OffsetDateTime::parse(
            "2026-09-19T09:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let mut stations = HashMap::new();
        stations.insert(
            BikeStationId::new("tdx:Taipei:A"),
            BikeStationAvailability::new(
                BikeStationId::new("tdx:Taipei:A"),
                2,
                0,
                true,
                now - Duration::seconds(30),
            ),
        );
        stations.insert(
            BikeStationId::new("tdx:Taipei:B"),
            BikeStationAvailability::new(
                BikeStationId::new("tdx:Taipei:B"),
                0,
                4,
                true,
                now - Duration::seconds(30),
            ),
        );
        let snapshot = BikeAvailabilitySnapshot::new(now, stations);
        let view = network.availability_view_at(&snapshot, now, Duration::seconds(120));

        assert_eq!(
            view.usable_pickups(&BoardingPointId::new("bp"))
                .iter()
                .map(|access| access.station_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tdx:Taipei:A"]
        );
        assert_eq!(
            view.usable_dropoffs(&BoardingPointId::new("bp"))
                .iter()
                .map(|access| access.station_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tdx:Taipei:B"]
        );
    }

    #[test]
    fn availability_view_rejects_stale_disabled_and_unknown_observations() {
        let station = BikeStation::new(
            BikeStationId::new("tdx:Taipei:A"),
            "A",
            coordinate(25.0, 121.5),
        );
        let network = BikeShareNetwork::new(vec![station])
            .with_access_links(vec![BikeStationAccess::new(
                BoardingPointId::new("bp"),
                BikeStationId::new("tdx:Taipei:A"),
                Some(BikeWalkingAccess::new(10, 50)),
                Some(BikeWalkingAccess::new(20, 80)),
            )])
            .with_revision("revision");
        let now = OffsetDateTime::parse(
            "2026-09-19T09:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let mut stations = HashMap::new();
        stations.insert(
            BikeStationId::new("tdx:Taipei:A"),
            BikeStationAvailability::new(
                BikeStationId::new("tdx:Taipei:A"),
                8,
                8,
                false,
                now - Duration::seconds(30),
            ),
        );
        stations.insert(
            BikeStationId::new("tdx:Taipei:UNKNOWN"),
            BikeStationAvailability::new(
                BikeStationId::new("tdx:Taipei:UNKNOWN"),
                8,
                8,
                true,
                now - Duration::seconds(30),
            ),
        );
        let disabled = BikeAvailabilitySnapshot::new(now, stations);
        let view = network.availability_view_at(&disabled, now, Duration::seconds(120));
        assert!(view.usable_pickups(&BoardingPointId::new("bp")).is_empty());
        assert!(view.usable_dropoffs(&BoardingPointId::new("bp")).is_empty());

        let mut stations = HashMap::new();
        stations.insert(
            BikeStationId::new("tdx:Taipei:A"),
            BikeStationAvailability::new(
                BikeStationId::new("tdx:Taipei:A"),
                8,
                8,
                true,
                now - Duration::seconds(121),
            ),
        );
        let stale = BikeAvailabilitySnapshot::new(now, stations);
        let view = network.availability_view_at(&stale, now, Duration::seconds(120));
        assert!(view.usable_pickups(&BoardingPointId::new("bp")).is_empty());
        assert!(view.usable_dropoffs(&BoardingPointId::new("bp")).is_empty());
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BikeAvailabilityError {
    #[error("bike-share availability provider returned a rejected payload")]
    RejectedPayload,
    #[error("bike-share availability provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("bike-share availability provider is rate limited")]
    RateLimited,
    #[error("bike-share availability provider configuration is invalid")]
    InvalidConfiguration,
}

impl BikeAvailabilityError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::TemporarilyUnavailable | Self::RateLimited)
    }
}

#[async_trait::async_trait]
pub trait BikeAvailabilitySource: Send + Sync {
    async fn snapshot(
        &self,
        station_ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError>;
}
