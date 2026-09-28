use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::domain::{
    bikeshare::BikeShareNetwork,
    transit::{TransitNetwork, TransitNetworkValidationError},
};
use crate::providers::bindings::TdxProviderBindings;
use crate::street::StreetRouter;

pub mod bike_access;
pub mod bikeshare_catalog;
pub mod gtfs;
pub mod static_transfers;
pub use gtfs::{GtfsBuildDiagnostics, GtfsBuildOutput};

pub const ARTIFACT_SCHEMA_VERSION: u32 = 7;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactCounts {
    pub stops: usize,
    pub routes: usize,
    pub patterns: usize,
    pub trips: usize,
    pub calendars: usize,
    pub frequencies: usize,
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
pub struct BuildDiagnostics {
    pub static_transfers: Option<static_transfers::TransferStatistics>,
    pub bike_access: Option<bike_access::BikeAccessDiagnostics>,
}

#[derive(Clone, Debug)]
pub struct BuildOutput {
    pub network: TransitNetwork,
    pub provider_bindings: TdxProviderBindings,
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
    #[error("GTFS archive contains no usable transit stops")]
    EmptyNetwork,
    #[error("artifact JSON is invalid: {0}")]
    InvalidArtifactJson(String),
    #[error("provider bindings are invalid: {0}")]
    InvalidProviderBindings(String),
    #[error("normalized network failed validation: {0}")]
    InvalidArtifact(#[from] TransitNetworkValidationError),
    #[error("bike-share artifact is invalid: {0}")]
    InvalidBikeShareArtifact(#[from] crate::domain::bikeshare::BikeShareNetworkValidationError),
    #[error("walking transfer preprocessing failed: {0}")]
    StreetRouting(#[from] crate::street::StreetRoutingError),
}

pub async fn build_from_gtfs_archive_with_street(
    input: &Path,
    output: &Path,
    street: &dyn StreetRouter,
) -> Result<GtfsBuildOutput, DataError> {
    build_from_gtfs_archive_with_catalog(
        input,
        output,
        BikeShareNetwork::new(Vec::new()),
        None,
        street,
    )
    .await
}

pub async fn build_from_gtfs_archive_with_tdx_bike_stations(
    input: &Path,
    output: &Path,
    taipei_stations: &Path,
    new_taipei_stations: &Path,
    street: &dyn StreetRouter,
) -> Result<GtfsBuildOutput, DataError> {
    let taipei_json = fs::read_to_string(taipei_stations)
        .map_err(|error| DataError::Io(taipei_stations.into(), error))?;
    let new_taipei_json = fs::read_to_string(new_taipei_stations)
        .map_err(|error| DataError::Io(new_taipei_stations.into(), error))?;
    let bike_share = bikeshare_catalog::parse_tdx_station_catalog(&taipei_json, &new_taipei_json)?;
    let sources = BTreeMap::from([
        (
            source_key("taipei", taipei_stations),
            hash_file(taipei_stations)?,
        ),
        (
            source_key("new-taipei", new_taipei_stations),
            hash_file(new_taipei_stations)?,
        ),
    ]);
    build_from_gtfs_archive_with_catalog(input, output, bike_share, Some(sources), street).await
}

async fn build_from_gtfs_archive_with_catalog(
    input: &Path,
    output: &Path,
    bike_share: BikeShareNetwork,
    supplementary_sources: Option<BTreeMap<String, String>>,
    street: &dyn StreetRouter,
) -> Result<GtfsBuildOutput, DataError> {
    let (mut network, provider_bindings, diagnostics) = gtfs::build_network(input)?;
    let static_transfers =
        static_transfers::enrich_network_with_static_transfers(&mut network, street).await?;
    let (bike_share, bike_access) = if bike_share.stations.is_empty() {
        (bike_share, None)
    } else {
        let (network, diagnostics) = bike_access::precompute_bike_access(
            &network,
            bike_share,
            street,
            bike_access::BikeAccessPolicy::default(),
        )
        .await?;
        (network, Some(diagnostics))
    };
    let build = finish_gtfs_build(
        input,
        output,
        network,
        provider_bindings,
        bike_share,
        supplementary_sources.unwrap_or_default(),
        BuildDiagnostics {
            static_transfers: Some(static_transfers),
            bike_access,
        },
    )?;
    Ok(GtfsBuildOutput { build, diagnostics })
}

fn finish_gtfs_build(
    input: &Path,
    output: &Path,
    network: TransitNetwork,
    provider_bindings: TdxProviderBindings,
    bike_share_network: BikeShareNetwork,
    supplementary_sources: BTreeMap<String, String>,
    diagnostics: BuildDiagnostics,
) -> Result<BuildOutput, DataError> {
    let static_revision = revision_for_network(&network, false)?;
    let schedule_revision = revision_for_network(&network, true)?;
    let network = network.with_revisions(static_revision.clone(), schedule_revision.clone());
    network.validate()?;
    provider_bindings
        .validate(&network)
        .map_err(DataError::InvalidProviderBindings)?;
    let bike_share_revision = revision_for_bike_share_network(&bike_share_network)?;
    let bike_share_network = bike_share_network.with_revision(bike_share_revision.clone());
    bike_share_network.validate()?;
    let source_hash = hash_file(input)?;
    let mut sources = BTreeMap::from([(
        input
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("gtfs.zip")
            .to_owned(),
        source_hash,
    )]);
    sources.extend(supplementary_sources);
    let manifest = ArtifactManifest {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        scope: "taiwan-gtfs".into(),
        built_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into()),
        raw_snapshot_date: None,
        sources,
        counts: ArtifactCounts {
            stops: network.stops.len(),
            routes: network.routes.len(),
            patterns: network.patterns.len(),
            trips: network.trips.len(),
            calendars: network.calendars.len(),
            frequencies: network.frequencies.len(),
            transfer_edges: network.transfer_edges.len(),
            bike_stations: bike_share_network.stations.len(),
            bike_access_links: bike_share_network.access_links.len(),
        },
        revisions: ArtifactRevisions {
            static_network_revision: static_revision,
            schedule_revision,
            bike_share_network_revision: bike_share_revision,
        },
    };
    write_network_artifact(output, &network)?;
    write_json_atomic(&output.join("provider-bindings.json"), &provider_bindings)?;
    write_bikeshare_artifact(output, &bike_share_network)?;
    write_json_atomic(&output.join("manifest.json"), &manifest)?;
    Ok(BuildOutput {
        network,
        provider_bindings,
        bike_share_network,
        manifest,
        diagnostics,
    })
}

fn source_key(city: &str, path: &Path) -> String {
    format!(
        "tdx-bike-stations/{city}/{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("station.json")
    )
}

fn hash_file(path: &Path) -> Result<String, DataError> {
    let mut file = fs::File::open(path).map_err(|error| DataError::Io(path.into(), error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| DataError::Io(path.into(), error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn load_network_artifact(path: &Path) -> Result<TransitNetwork, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    TransitNetwork::from_artifact_json(&body)
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))
}

pub fn load_provider_bindings_artifact(path: &Path) -> Result<TdxProviderBindings, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    serde_json::from_str(&body).map_err(|error| DataError::InvalidArtifactJson(error.to_string()))
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
        network.routes.clear();
        network.patterns.clear();
        network.transfer_edges.clear();
    } else {
        network.trips.clear();
        network.calendars.clear();
        network.frequencies.clear();
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
