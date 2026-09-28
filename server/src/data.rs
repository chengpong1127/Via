use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::data::via_static::{ViaStaticCounts, ViaStaticRevisions, ViaStaticSource};
use crate::domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork};
use crate::street::StreetRouter;

pub mod bike_access;
pub mod bikeshare_catalog;
pub mod fetch;
pub mod gtfs;
pub mod static_transfers;
pub mod via_static;
pub use gtfs::{GtfsBuildDiagnostics, GtfsBuildOutput};
pub use via_static::ViaStatic;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildDiagnostics {
    pub static_transfers: Option<static_transfers::TransferStatistics>,
    pub bike_access: Option<bike_access::BikeAccessDiagnostics>,
}

#[derive(Clone, Debug)]
pub struct BuildOutput {
    pub via_static: ViaStatic,
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
    #[error("fetch snapshot is invalid: {0}")]
    InvalidFetchSnapshot(String),
    #[error("ViaStatic artifact is invalid: {0}")]
    InvalidViaStatic(#[from] crate::data::via_static::ViaStaticValidationError),
    #[error("walking transfer preprocessing failed: {0}")]
    StreetRouting(#[from] crate::street::StreetRoutingError),
}

pub async fn build_from_fetch_snapshot(
    input: &Path,
    output: &Path,
    street: &dyn StreetRouter,
) -> Result<GtfsBuildOutput, DataError> {
    let fetch_manifest_path = input.join("manifest.json");
    let fetch_manifest_body = fs::read_to_string(&fetch_manifest_path)
        .map_err(|error| DataError::Io(fetch_manifest_path.clone(), error))?;
    let fetch_manifest = serde_json::from_str::<fetch::FetchManifest>(&fetch_manifest_body)
        .map_err(|error| DataError::InvalidFetchSnapshot(error.to_string()))?;
    if fetch_manifest.manifest_version != 1 || fetch_manifest.fetched_at.trim().is_empty() {
        return Err(DataError::InvalidFetchSnapshot(
            "unsupported manifest version or missing fetch timestamp".to_owned(),
        ));
    }
    for required in ["gtfs", "bike-stations-taipei", "bike-stations-new-taipei"] {
        if !fetch_manifest.sources.contains_key(required) {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "missing {required} provenance"
            )));
        }
    }
    let mut expected_files = std::collections::HashSet::from(["manifest.json".to_owned()]);
    for (source_name, source) in &fetch_manifest.sources {
        let filename = fetch::source_file_name(source_name).ok_or_else(|| {
            DataError::InvalidFetchSnapshot(format!("unknown source {source_name}"))
        })?;
        expected_files.insert(filename.clone());
        let source_path = input.join(filename);
        if source.byte_size
            != fs::metadata(&source_path)
                .map_err(|error| DataError::Io(source_path.clone(), error))?
                .len()
            || source.sha256 != sha256_file(&source_path)?
        {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "{source_name} does not match its fetch provenance"
            )));
        }
    }
    for entry in fs::read_dir(input).map_err(|error| DataError::Io(input.into(), error))? {
        let entry = entry.map_err(|error| DataError::Io(input.into(), error))?;
        let filename = entry.file_name().to_string_lossy().into_owned();
        if !expected_files.contains(&filename) {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "untracked source file {filename}"
            )));
        }
    }
    let gtfs_path = input.join("gtfs.zip");
    let taipei_pages = read_station_pages(input, &fetch_manifest, "bike-stations-taipei")?;
    let new_taipei_pages = read_station_pages(input, &fetch_manifest, "bike-stations-new-taipei")?;
    let bike_share =
        bikeshare_catalog::parse_tdx_station_catalog_pages(&taipei_pages, &new_taipei_pages)?;
    let sources = fetch_manifest
        .sources
        .iter()
        .map(|(name, source)| {
            (
                name.clone(),
                ViaStaticSource {
                    source_id: source.source_id.clone(),
                    source_url: source.url.clone(),
                    sha256: source.sha256.clone(),
                    byte_size: source.byte_size,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let (mut network, provider_bindings, diagnostics) = gtfs::build_network(&gtfs_path)?;
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
        output,
        network,
        provider_bindings,
        bike_share,
        fetch_manifest.fetched_at,
        sources,
        BuildDiagnostics {
            static_transfers: Some(static_transfers),
            bike_access,
        },
    )?;
    Ok(GtfsBuildOutput { build, diagnostics })
}

fn read_station_pages(
    input: &Path,
    manifest: &fetch::FetchManifest,
    source_prefix: &str,
) -> Result<Vec<String>, DataError> {
    let mut source_names = manifest
        .sources
        .keys()
        .filter(|name| {
            *name == source_prefix || name.starts_with(&format!("{source_prefix}-page-"))
        })
        .cloned()
        .collect::<Vec<_>>();
    source_names.sort();
    if source_names
        .first()
        .is_none_or(|name| name != source_prefix)
    {
        return Err(DataError::InvalidFetchSnapshot(format!(
            "missing {source_prefix} source data"
        )));
    }
    source_names
        .iter()
        .map(|source_name| {
            let filename = fetch::source_file_name(source_name).ok_or_else(|| {
                DataError::InvalidFetchSnapshot(format!("invalid source {source_name}"))
            })?;
            let path = input.join(filename);
            fs::read_to_string(&path).map_err(|error| DataError::Io(path, error))
        })
        .collect()
}

fn finish_gtfs_build(
    output: &Path,
    network: TransitNetwork,
    provider_bindings: crate::providers::bindings::TdxProviderBindings,
    bike_share_network: BikeShareNetwork,
    fetched_at: String,
    sources: BTreeMap<String, ViaStaticSource>,
    diagnostics: BuildDiagnostics,
) -> Result<BuildOutput, DataError> {
    let static_revision = revision_for_network(&network, false)?;
    let schedule_revision = revision_for_network(&network, true)?;
    let network = network.with_revisions(static_revision.clone(), schedule_revision.clone());
    let bike_share_revision = revision_for_bike_share_network(&bike_share_network)?;
    let bike_share_network = bike_share_network.with_revision(bike_share_revision.clone());
    let via_static = ViaStatic {
        schema_version: crate::data::via_static::VIA_STATIC_SCHEMA_VERSION,
        fetched_at,
        sources,
        revisions: ViaStaticRevisions {
            transit_static: static_revision,
            schedule: schedule_revision,
            bike_share: bike_share_revision,
        },
        counts: ViaStaticCounts {
            stops: network.stops.len(),
            routes: network.routes.len(),
            patterns: network.patterns.len(),
            trips: network.trips.len(),
            calendars: network.calendars.len(),
            frequencies: network.frequencies.len(),
            transfers: network.transfer_edges.len(),
            bike_stations: bike_share_network.stations.len(),
            bike_access_links: bike_share_network.access_links.len(),
        },
        transit: network,
        provider_bindings,
        bike_share: bike_share_network,
    };
    via_static.validate()?;
    write_json_atomic(&output.join(".via-static.json"), &via_static)?;
    Ok(BuildOutput {
        via_static,
        diagnostics,
    })
}

fn sha256_file(path: &Path) -> Result<String, DataError> {
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

pub fn load_static_artifact(path: &Path) -> Result<ViaStatic, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    ViaStatic::from_json(&body).map_err(DataError::InvalidViaStatic)
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
