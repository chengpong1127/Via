use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::data::{DataError, fetch::sha256_hex};
use crate::domain::{bikeshare::BikeShareNetwork, transit::network::TransitNetwork};
use crate::providers::tdx_bindings::TdxProviderBindings;

pub const VIA_STATIC_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStaticSource {
    pub source_id: String,
    #[serde(alias = "url")]
    pub source_url: Option<String>,
    pub sha256: String,
    pub byte_size: u64,
}

impl ViaStaticSource {
    pub fn has_valid_sha256(&self) -> bool {
        self.sha256.len() == 64 && self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStaticCounts {
    pub stops: usize,
    pub routes: usize,
    pub patterns: usize,
    pub trips: usize,
    pub calendars: usize,
    pub frequencies: usize,
    pub transfers: usize,
    pub bike_stations: usize,
    pub bike_access_links: usize,
}

impl ViaStaticCounts {
    pub fn of(transit: &TransitNetwork, bike_share: &BikeShareNetwork) -> Self {
        Self {
            stops: transit.stops.len(),
            routes: transit.routes.len(),
            patterns: transit.patterns.len(),
            trips: transit.trips.len(),
            calendars: transit.calendars.len(),
            frequencies: transit.frequencies.len(),
            transfers: transit.transfer_edges.len(),
            bike_stations: bike_share.stations.len(),
            bike_access_links: bike_share.access_links.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStaticRevisions {
    pub transit_static: String,
    pub schedule: String,
    pub bike_share: String,
}

impl ViaStaticRevisions {
    pub fn of(transit: &TransitNetwork, bike_share: &BikeShareNetwork) -> Self {
        Self {
            transit_static: transit.static_network_revision.clone(),
            schedule: transit.schedule_revision.clone(),
            bike_share: bike_share.static_network_revision.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStatic {
    pub schema_version: u32,
    pub fetched_at: String,
    pub sources: BTreeMap<String, ViaStaticSource>,
    pub revisions: ViaStaticRevisions,
    pub counts: ViaStaticCounts,
    pub transit: TransitNetwork,
    pub provider_bindings: TdxProviderBindings,
    pub bike_share: BikeShareNetwork,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ViaStaticValidationError {
    #[error("unsupported ViaStatic schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("fetch timestamp is missing")]
    MissingFetchTimestamp,
    #[error("fetch timestamp is not a valid RFC3339 timestamp: {0}")]
    InvalidFetchTimestamp(String),
    #[error("ViaStatic JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("source provenance is invalid for {0}")]
    InvalidSource(String),
    #[error("static counts do not match the composed networks")]
    CountsMismatch,
    #[error("revisions do not match the composed networks")]
    RevisionsMismatch,
    #[error("provider bindings are invalid: {0}")]
    InvalidProviderBindings(String),
    #[error("Bike access link references an unknown transit boarding point {0}")]
    UnknownTransitBoardingPoint(String),
    #[error("transit network is invalid: {0}")]
    InvalidTransitNetwork(String),
    #[error("BikeShare network is invalid: {0}")]
    InvalidBikeShareNetwork(String),
}

impl ViaStatic {
    pub fn compose(
        transit: TransitNetwork,
        provider_bindings: TdxProviderBindings,
        bike_share: BikeShareNetwork,
        fetched_at: String,
        sources: BTreeMap<String, ViaStaticSource>,
    ) -> Result<Self, DataError> {
        let static_revision = static_network_revision(&transit)?;
        let schedule = schedule_revision(&transit)?;
        let transit = transit.with_revisions(static_revision, schedule);
        let bike_revision = bike_share_revision(&bike_share)?;
        let bike_share = bike_share.with_revision(bike_revision);
        let via_static = Self {
            schema_version: VIA_STATIC_SCHEMA_VERSION,
            fetched_at,
            sources,
            revisions: ViaStaticRevisions::of(&transit, &bike_share),
            counts: ViaStaticCounts::of(&transit, &bike_share),
            transit,
            provider_bindings,
            bike_share,
        };
        via_static.validate()?;
        Ok(via_static)
    }

    pub fn validate(&self) -> Result<(), ViaStaticValidationError> {
        if self.schema_version != VIA_STATIC_SCHEMA_VERSION {
            return Err(ViaStaticValidationError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.fetched_at.trim().is_empty() {
            return Err(ViaStaticValidationError::MissingFetchTimestamp);
        }
        if OffsetDateTime::parse(&self.fetched_at, &Rfc3339).is_err() {
            return Err(ViaStaticValidationError::InvalidFetchTimestamp(
                self.fetched_at.clone(),
            ));
        }
        for (name, source) in &self.sources {
            if name.trim().is_empty()
                || source.source_id.trim().is_empty()
                || source.byte_size == 0
                || !source.has_valid_sha256()
                || source.source_url.as_ref().is_some_and(|url| {
                    url.contains("client_secret") || url.contains("access_token")
                })
            {
                return Err(ViaStaticValidationError::InvalidSource(name.clone()));
            }
        }
        self.transit
            .validate()
            .map_err(|error| ViaStaticValidationError::InvalidTransitNetwork(error.to_string()))?;
        self.provider_bindings
            .validate(&self.transit)
            .map_err(ViaStaticValidationError::InvalidProviderBindings)?;
        self.bike_share.validate().map_err(|error| {
            ViaStaticValidationError::InvalidBikeShareNetwork(error.to_string())
        })?;
        let transit_stops = self
            .transit
            .stops
            .iter()
            .map(|stop| stop.id.as_str())
            .collect::<HashSet<_>>();
        for link in &self.bike_share.access_links {
            if !transit_stops.contains(link.boarding_point_id.as_str()) {
                return Err(ViaStaticValidationError::UnknownTransitBoardingPoint(
                    link.boarding_point_id.as_str().to_owned(),
                ));
            }
        }
        if self.revisions != ViaStaticRevisions::of(&self.transit, &self.bike_share) {
            return Err(ViaStaticValidationError::RevisionsMismatch);
        }
        if self.counts != ViaStaticCounts::of(&self.transit, &self.bike_share) {
            return Err(ViaStaticValidationError::CountsMismatch);
        }
        Ok(())
    }

    pub fn from_json(input: &str) -> Result<Self, ViaStaticValidationError> {
        let artifact = serde_json::from_str::<Self>(input)
            .map_err(|error| ViaStaticValidationError::InvalidJson(error.to_string()))?;
        artifact.validate()?;
        Ok(artifact)
    }
}

fn static_network_revision(network: &TransitNetwork) -> Result<String, DataError> {
    let mut network = network.clone();
    network.static_network_revision.clear();
    network.schedule_revision.clear();
    network.trips.clear();
    network.calendars.clear();
    network.frequencies.clear();
    content_revision(&network.canonicalize())
}

fn schedule_revision(network: &TransitNetwork) -> Result<String, DataError> {
    let mut network = network.clone();
    network.static_network_revision.clear();
    network.schedule_revision.clear();
    network.stops.clear();
    network.routes.clear();
    network.patterns.clear();
    network.transfer_edges.clear();
    content_revision(&network.canonicalize())
}

fn bike_share_revision(network: &BikeShareNetwork) -> Result<String, DataError> {
    let mut network = network.clone();
    network.static_network_revision.clear();
    content_revision(&network.canonicalize())
}

fn content_revision(value: &impl Serialize) -> Result<String, DataError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
    Ok(format!("sha256:{}", sha256_hex(&bytes)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{
        domain::{
            bikeshare::{BikeStation, BikeStationAccess, BikeWalkingAccess},
            ids::{BikeStationId, BoardingPointId, TransitPatternId},
            location::GeoCoordinate,
            transit::network::{TransitStop, default_service_day_boundary},
        },
        providers::tdx_bindings::{BusPatternBinding, TdxProviderBindings},
    };

    fn artifact() -> ViaStatic {
        let transit = TransitNetwork::new(
            default_service_day_boundary(),
            vec![TransitStop::new(
                BoardingPointId::new("stop"),
                "Stop",
                GeoCoordinate {
                    latitude: 25.0,
                    longitude: 121.0,
                },
            )],
            Vec::new(),
            Vec::new(),
        )
        .with_revisions("transit-static", "schedule");
        ViaStatic {
            schema_version: VIA_STATIC_SCHEMA_VERSION,
            fetched_at: "2026-09-28T00:00:00Z".to_owned(),
            sources: BTreeMap::from([(
                "gtfs".to_owned(),
                ViaStaticSource {
                    source_id: "local-gtfs-file".to_owned(),
                    source_url: None,
                    sha256: "a".repeat(64),
                    byte_size: 10,
                },
            )]),
            revisions: ViaStaticRevisions {
                transit_static: "transit-static".to_owned(),
                schedule: "schedule".to_owned(),
                bike_share: "bike-share".to_owned(),
            },
            counts: ViaStaticCounts {
                stops: 1,
                ..ViaStaticCounts::default()
            },
            transit,
            provider_bindings: TdxProviderBindings::default(),
            bike_share: BikeShareNetwork::new(Vec::new()).with_revision("bike-share"),
        }
    }

    #[test]
    fn via_static_serializes_and_validates_as_one_artifact() {
        let artifact = artifact();
        artifact.validate().unwrap();
        let json = serde_json::to_string(&artifact).unwrap();
        let restored = ViaStatic::from_json(&json).unwrap();
        assert_eq!(restored.schema_version, VIA_STATIC_SCHEMA_VERSION);
        assert_eq!(restored.transit.static_network_revision, "transit-static");
        assert_eq!(restored.bike_share.static_network_revision, "bike-share");
    }

    #[test]
    fn via_static_rejects_unsupported_schema_versions() {
        for version in [1, VIA_STATIC_SCHEMA_VERSION + 1] {
            let mut value = serde_json::to_value(artifact()).unwrap();
            value["schema_version"] = serde_json::Value::from(version);
            assert_eq!(
                ViaStatic::from_json(&value.to_string()).unwrap_err(),
                ViaStaticValidationError::UnsupportedSchemaVersion(version),
                "schema version {version}"
            );
        }
    }

    #[test]
    fn via_static_rejects_invalid_rfc3339_fetch_timestamps() {
        let mut artifact = artifact();
        artifact.fetched_at = "2026-99-28T00:00:00Z".to_owned();
        assert!(matches!(
            artifact.validate(),
            Err(ViaStaticValidationError::InvalidFetchTimestamp(_))
        ));
    }

    #[test]
    fn via_static_json_rejects_unknown_top_level_fields() {
        let mut value = serde_json::to_value(artifact()).unwrap();
        value["stale_field"] = serde_json::Value::Bool(true);
        assert!(matches!(
            ViaStatic::from_json(&value.to_string()),
            Err(ViaStaticValidationError::InvalidJson(_))
        ));
    }

    #[test]
    fn via_static_json_rejects_unknown_nested_persisted_fields() {
        let mut value = serde_json::to_value(artifact()).unwrap();
        value["sources"]["gtfs"]["stale_source_field"] = serde_json::Value::Bool(true);
        assert!(matches!(
            ViaStatic::from_json(&value.to_string()),
            Err(ViaStaticValidationError::InvalidJson(_))
        ));

        let mut value = serde_json::to_value(artifact()).unwrap();
        value["bike_share"]["stale_network_field"] = serde_json::Value::Bool(true);
        assert!(matches!(
            ViaStatic::from_json(&value.to_string()),
            Err(ViaStaticValidationError::InvalidJson(_))
        ));
    }

    #[test]
    fn via_static_validates_provider_and_bike_access_references() {
        let mut invalid_binding = artifact();
        invalid_binding.provider_bindings = TdxProviderBindings {
            bus_patterns: HashMap::from([(
                TransitPatternId::new("missing-pattern"),
                BusPatternBinding {
                    tdx_service_id: "service".to_owned(),
                    realtime_scope: "Taipei".to_owned(),
                },
            )]),
            ..TdxProviderBindings::default()
        };
        assert!(matches!(
            invalid_binding.validate(),
            Err(ViaStaticValidationError::InvalidProviderBindings(_))
        ));

        let mut invalid_access = artifact();
        let station_id = BikeStationId::new("station");
        invalid_access.bike_share = BikeShareNetwork::new(vec![BikeStation::new(
            station_id.clone(),
            "Station",
            GeoCoordinate {
                latitude: 25.0,
                longitude: 121.0,
            },
        )])
        .with_access_links(vec![BikeStationAccess::new(
            BoardingPointId::new("missing-stop"),
            station_id,
            Some(BikeWalkingAccess::new(60, 100)),
            None,
        )])
        .with_revision("bike-share");
        invalid_access.counts.bike_stations = 1;
        invalid_access.counts.bike_access_links = 1;
        assert!(matches!(
            invalid_access.validate(),
            Err(ViaStaticValidationError::UnknownTransitBoardingPoint(_))
        ));
    }

    #[test]
    fn via_static_rejects_unknown_bike_stations_at_the_network_boundary() {
        let mut invalid = artifact();
        invalid.bike_share = invalid
            .bike_share
            .with_access_links(vec![BikeStationAccess::new(
                BoardingPointId::new("stop"),
                BikeStationId::new("missing-station"),
                Some(BikeWalkingAccess::new(60, 100)),
                None,
            )]);
        invalid.counts.bike_access_links = 1;
        let expected = ViaStaticValidationError::InvalidBikeShareNetwork(
            crate::domain::bikeshare::BikeShareNetworkValidationError::UnknownStation(
                BikeStationId::new("missing-station"),
            )
            .to_string(),
        );
        assert_eq!(invalid.validate().unwrap_err(), expected);
        assert_eq!(
            ViaStatic::from_json(&serde_json::to_string(&invalid).unwrap()).unwrap_err(),
            expected
        );
    }

    #[test]
    fn source_reads_fetch_manifests_written_with_url() {
        let source: ViaStaticSource = serde_json::from_str(
            r#"{"source_id":"gtfs","url":"https://example.test/gtfs.zip","sha256":"abc","byte_size":3}"#,
        )
        .unwrap();
        assert_eq!(
            source.source_url.as_deref(),
            Some("https://example.test/gtfs.zip")
        );
    }
}
