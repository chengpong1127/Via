use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;

use crate::data::via_static::{ViaStaticCounts, ViaStaticRevisions, ViaStaticSource};
use crate::domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork};
use crate::street::StreetRouter;

pub mod bike_access;
pub mod bikeshare_catalog;
pub mod fetch;
pub mod gtfs;
pub mod static_transfers;
pub mod via_static;
pub use gtfs::{GtfsBuildDiagnostics, GtfsBuildOutput, GtfsModeCoverage};
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
    #[error("GTFS archive {path} contains no usable route service: {details}")]
    NoUsableRouteService { path: PathBuf, details: String },
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
    let fetch_inputs = read_fetch_inputs(input)?;
    let bike_share = bikeshare_catalog::parse_tdx_station_catalog(
        &fetch_inputs.taipei_stations,
        &fetch_inputs.new_taipei_stations,
    )?;
    let sources = fetch_inputs
        .manifest
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

    let (mut network, provider_bindings, diagnostics) =
        gtfs::build_network(&fetch_inputs.gtfs_path)?;
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
        fetch_inputs.manifest.fetched_at,
        sources,
        BuildDiagnostics {
            static_transfers: Some(static_transfers),
            bike_access,
        },
    )?;
    Ok(GtfsBuildOutput { build, diagnostics })
}

struct FetchInputs {
    manifest: fetch::FetchManifest,
    gtfs_path: PathBuf,
    taipei_stations: String,
    new_taipei_stations: String,
}

fn read_fetch_inputs(input: &Path) -> Result<FetchInputs, DataError> {
    let required_files = [
        "gtfs.zip",
        "bike-stations-taipei.json",
        "bike-stations-new-taipei.json",
        "manifest.json",
    ];
    let required_files = required_files
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let mut actual_files = std::collections::HashSet::new();
    for entry in fs::read_dir(input).map_err(|error| DataError::Io(input.into(), error))? {
        let entry = entry.map_err(|error| DataError::Io(input.into(), error))?;
        let filename = entry.file_name().into_string().map_err(|_| {
            DataError::InvalidFetchSnapshot("snapshot contains a non-UTF-8 filename".to_owned())
        })?;
        if !required_files.contains(filename.as_str()) {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "untracked source file {filename}"
            )));
        }
        if !entry
            .file_type()
            .map_err(|error| DataError::Io(input.into(), error))?
            .is_file()
        {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "snapshot entry {filename} is not a regular file"
            )));
        }
        actual_files.insert(filename);
    }
    if actual_files.len() != required_files.len() {
        let missing = required_files
            .iter()
            .filter(|filename| !actual_files.contains(**filename))
            .copied()
            .collect::<Vec<_>>();
        return Err(DataError::InvalidFetchSnapshot(format!(
            "snapshot is missing required files: {}",
            missing.join(", ")
        )));
    }

    let manifest_path = input.join("manifest.json");
    let manifest_body = fs::read_to_string(&manifest_path)
        .map_err(|error| DataError::Io(manifest_path.clone(), error))?;
    let manifest = serde_json::from_str::<fetch::FetchManifest>(&manifest_body)
        .map_err(|error| DataError::InvalidFetchSnapshot(error.to_string()))?;
    if manifest.manifest_version != 1
        || time::OffsetDateTime::parse(&manifest.fetched_at, &Rfc3339).is_err()
    {
        return Err(DataError::InvalidFetchSnapshot(
            "unsupported manifest version or invalid fetch timestamp".to_owned(),
        ));
    }
    let required_sources = ["gtfs", "bike-stations-taipei", "bike-stations-new-taipei"];
    if manifest.sources.len() != required_sources.len()
        || required_sources
            .iter()
            .any(|name| !manifest.sources.contains_key(*name))
    {
        return Err(DataError::InvalidFetchSnapshot(
            "manifest must contain exactly the gtfs, Taipei, and NewTaipei sources".to_owned(),
        ));
    }
    for (source_name, source) in &manifest.sources {
        if source.source_id.trim().is_empty()
            || source.sha256.len() != 64
            || !source.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "{source_name} has invalid provenance metadata"
            )));
        }
        let filename = fetch::source_file_name(source_name).ok_or_else(|| {
            DataError::InvalidFetchSnapshot(format!("unknown source {source_name}"))
        })?;
        let source_path = input.join(filename);
        let metadata = fs::metadata(&source_path)
            .map_err(|error| DataError::Io(source_path.clone(), error))?;
        if source.byte_size != metadata.len() || source.sha256 != sha256_file(&source_path)? {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "{source_name} does not match its fetch provenance"
            )));
        }
    }

    let gtfs_path = input.join("gtfs.zip");
    let taipei_path = input.join("bike-stations-taipei.json");
    let new_taipei_path = input.join("bike-stations-new-taipei.json");
    Ok(FetchInputs {
        manifest,
        gtfs_path,
        taipei_stations: fs::read_to_string(&taipei_path)
            .map_err(|error| DataError::Io(taipei_path, error))?,
        new_taipei_stations: fs::read_to_string(&new_taipei_path)
            .map_err(|error| DataError::Io(new_taipei_path, error))?,
    })
}

fn finish_gtfs_build(
    output: &Path,
    network: TransitNetwork,
    provider_bindings: crate::providers::tdx_bindings::TdxProviderBindings,
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
