use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use thiserror::Error;

use crate::street::StreetRouter;

pub mod bike_access;
pub mod bikeshare_catalog;
pub mod bus_headways;
pub mod bus_routes;
pub mod fetch;
pub mod gtfs;
pub mod static_transfers;
pub mod via_static;
pub use gtfs::{GtfsBuildDiagnostics, GtfsModeCoverage};
pub use via_static::ViaStatic;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildDiagnostics {
    pub gtfs: GtfsBuildDiagnostics,
    pub static_transfers: Option<static_transfers::TransferStatistics>,
    pub bike_access: Option<bike_access::BikeAccessDiagnostics>,
    pub bus_parent_routes: usize,
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
    bike_policy: bike_access::BikeAccessPolicy,
    transfer_policy: static_transfers::TransferSettings,
    same_stop_transfer_seconds: u32,
) -> Result<BuildOutput, DataError> {
    let snapshot = fetch::read_snapshot(input)?;
    let bike_share = bikeshare_catalog::parse_tdx_station_catalog(
        snapshot
            .bike_station_catalogs
            .iter()
            .map(|(city, body)| (city.as_str(), body.as_str())),
    )?;

    let (mut network, mut provider_bindings, gtfs) = gtfs::build_network(&snapshot.gtfs_path)?;
    let bus_parent_routes =
        bus_routes::attach_parent_routes(&mut provider_bindings, &snapshot.bus_routes);
    network.same_stop_transfer_seconds = same_stop_transfer_seconds;
    let static_transfers = static_transfers::enrich_network_with_static_transfers(
        &mut network,
        street,
        transfer_policy,
    )
    .await?;
    let (bike_share, bike_access) = if bike_share.stations.is_empty() {
        (bike_share, None)
    } else {
        let (network, diagnostics) =
            bike_access::precompute_bike_access(&network, bike_share, street, bike_policy).await?;
        (network, Some(diagnostics))
    };
    let via_static = ViaStatic::compose(
        network,
        provider_bindings,
        bike_share,
        snapshot.manifest.fetched_at,
        snapshot.manifest.sources,
    )?;
    write_json_atomic(&output.join(".via-static.json"), &via_static)?;
    Ok(BuildOutput {
        via_static,
        diagnostics: BuildDiagnostics {
            gtfs,
            static_transfers: Some(static_transfers),
            bike_access,
            bus_parent_routes,
        },
    })
}

pub fn load_static_artifact(path: &Path) -> Result<ViaStatic, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    ViaStatic::from_json(&body).map_err(DataError::InvalidViaStatic)
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
