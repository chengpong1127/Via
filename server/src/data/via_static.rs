use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork};
use crate::providers::bindings::TdxProviderBindings;

pub const VIA_STATIC_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStaticSource {
    pub source_id: String,
    pub source_url: Option<String>,
    pub sha256: String,
    pub byte_size: u64,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStaticRevisions {
    pub transit_static: String,
    pub schedule: String,
    pub bike_share: String,
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
    #[error("Bike access link references an unknown station {0}")]
    UnknownBikeStation(String),
    #[error("transit network is invalid: {0}")]
    InvalidTransitNetwork(String),
    #[error("BikeShare network is invalid: {0}")]
    InvalidBikeShareNetwork(String),
}

impl ViaStatic {
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
                || source.sha256.len() != 64
                || !source.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
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
        let bike_stations = self
            .bike_share
            .stations
            .iter()
            .map(|station| station.id.as_str())
            .collect::<HashSet<_>>();
        for link in &self.bike_share.access_links {
            if !transit_stops.contains(link.boarding_point_id.as_str()) {
                return Err(ViaStaticValidationError::UnknownTransitBoardingPoint(
                    link.boarding_point_id.as_str().to_owned(),
                ));
            }
            if !bike_stations.contains(link.station_id.as_str()) {
                return Err(ViaStaticValidationError::UnknownBikeStation(
                    link.station_id.as_str().to_owned(),
                ));
            }
        }
        if self.revisions.transit_static != self.transit.static_network_revision
            || self.revisions.schedule != self.transit.schedule_revision
            || self.revisions.bike_share != self.bike_share.static_network_revision
        {
            return Err(ViaStaticValidationError::RevisionsMismatch);
        }
        if self.counts
            != (ViaStaticCounts {
                stops: self.transit.stops.len(),
                routes: self.transit.routes.len(),
                patterns: self.transit.patterns.len(),
                trips: self.transit.trips.len(),
                calendars: self.transit.calendars.len(),
                frequencies: self.transit.frequencies.len(),
                transfers: self.transit.transfer_edges.len(),
                bike_stations: self.bike_share.stations.len(),
                bike_access_links: self.bike_share.access_links.len(),
            })
        {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{
        domain::{
            bikeshare::{BikeStation, BikeStationAccess, BikeWalkingAccess},
            ids::{BikeStationId, BoardingPointId, TransitPatternId},
            location::RequestCoordinate,
            transit::{TransitStop, default_service_day_boundary},
        },
        providers::bindings::{BusPatternBinding, TdxProviderBindings},
    };

    fn artifact() -> ViaStatic {
        let transit = TransitNetwork::new(
            default_service_day_boundary(),
            vec![TransitStop::new(
                BoardingPointId::new("stop"),
                "Stop",
                RequestCoordinate {
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
        let mut value = serde_json::to_value(artifact()).unwrap();
        value["schema_version"] = serde_json::Value::from(VIA_STATIC_SCHEMA_VERSION + 1);
        assert!(matches!(
            ViaStatic::from_json(&value.to_string()),
            Err(ViaStaticValidationError::UnsupportedSchemaVersion(2))
        ));
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
            RequestCoordinate {
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
}
