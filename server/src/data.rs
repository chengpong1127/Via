//! Static transit artifacts built from the local GTFS archive.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::domain::{
    bikeshare::BikeShareNetwork,
    transit::{TransitNetwork, TransitNetworkValidationError},
};

pub mod bike_access;
pub mod gtfs;
pub mod static_transfers;
pub use gtfs::{
    GtfsBuildDiagnostics, GtfsBuildOutput, build_from_gtfs_archive,
    build_from_gtfs_archive_with_street,
};

pub const ARTIFACT_SCHEMA_VERSION: u32 = 6;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactCounts {
    pub stops: usize,
    pub services: usize,
    pub scheduled_trips: usize,
    pub transfer_edges: usize,
    #[serde(default)]
    pub bike_stations: usize,
    #[serde(default)]
    pub bike_access_links: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRevisions {
    pub static_network_revision: String,
    pub schedule_revision: String,
    #[serde(default)]
    pub bike_share_network_revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifest {
    pub schema_version: u32,
    pub scope: String,
    pub built_at: String,
    pub raw_snapshot_date: Option<String>,
    pub sources: BTreeMap<String, String>,
    pub counts: ArtifactCounts,
    pub revisions: ArtifactRevisions,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildDiagnostics;

#[derive(Clone, Debug)]
pub struct BuildOutput {
    pub network: TransitNetwork,
    pub bike_share_network: BikeShareNetwork,
    pub manifest: ArtifactManifest,
    pub diagnostics: BuildDiagnostics,
}

#[derive(Debug, Error)]
pub enum DataError {
    #[error("data path {0} could not be read or written: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("GTFS archive {path} is invalid: {detail}")]
    InvalidGtfsArchive { path: PathBuf, detail: String },
    #[error("GTFS file {path} has malformed {kind}: {detail}")]
    MalformedGtfs {
        path: PathBuf,
        kind: &'static str,
        detail: String,
    },
    #[error("GTFS archive contains no usable transit stops")]
    EmptyNetwork,
    #[error("artifact JSON is invalid: {0}")]
    InvalidArtifactJson(String),
    #[error("normalized network failed validation: {0}")]
    InvalidArtifact(#[from] TransitNetworkValidationError),
    #[error("bike-share artifact is invalid: {0}")]
    InvalidBikeShareArtifact(#[from] crate::domain::bikeshare::BikeShareNetworkValidationError),
    #[error("walking transfer preprocessing failed: {0}")]
    StreetRouting(#[from] crate::street::StreetRoutingError),
}

pub fn load_network_artifact(path: &Path) -> Result<TransitNetwork, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    TransitNetwork::from_artifact_json(&body)
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))
}

pub fn load_bikeshare_artifact(path: &Path) -> Result<BikeShareNetwork, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    BikeShareNetwork::from_artifact_json(&body)
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))
}

pub fn load_manifest(path: &Path) -> Result<ArtifactManifest, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    serde_json::from_str(&body).map_err(|error| DataError::InvalidArtifactJson(error.to_string()))
}

pub fn write_network_artifact(output: &Path, network: &TransitNetwork) -> Result<(), DataError> {
    write_json_atomic(&output.join("transit-network.json"), network)
}

pub fn write_bikeshare_artifact(
    output: &Path,
    network: &BikeShareNetwork,
) -> Result<(), DataError> {
    write_json_atomic(&output.join("bikeshare-network.json"), network)
}

fn revision_for_network(network: &TransitNetwork, schedule: bool) -> Result<String, DataError> {
    let mut network = network.clone();
    network.static_network_revision.clear();
    network.schedule_revision.clear();
    if schedule {
        network.stops.clear();
        network.services.clear();
        network.transfer_edges.clear();
    } else {
        network.trips.clear();
        network.frequencies.clear();
        network.metro_timings.clear();
        network.derived_bus_travel_times.clear();
    }
    let bytes = serde_json::to_vec(&network.canonicalize())
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
    Ok(format!("sha256:{}", sha256_hex(&bytes)))
}

fn revision_for_bike_share_network(network: &BikeShareNetwork) -> Result<String, DataError> {
    let mut network = network.clone();
    network.static_network_revision.clear();
    let bytes = serde_json::to_vec(&network.canonicalize())
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
    Ok(format!("sha256:{}", sha256_hex(&bytes)))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), DataError> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DataError::Io(parent.into(), error))?;
    }
    let temporary = path.with_extension(format!(
        "{}tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
    ));
    fs::write(&temporary, body).map_err(|error| DataError::Io(temporary.clone(), error))?;
    fs::rename(&temporary, path).map_err(|error| DataError::Io(path.into(), error))
}
