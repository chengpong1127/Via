//! Static transit artifacts built from the local GTFS archive.
//!
//! The archive is read in place. Realtime providers remain responsible for
//! live timing and service conditions; derived Bus costs are static ranking
//! inputs and are never written into scheduled stop times.

mod archive;
mod diagnostics;
mod identity;
mod normalize;
mod reader;
mod routes;
mod schedule;
mod segment_profile;
mod shapes;
mod stops;

pub use diagnostics::GtfsBuildDiagnostics;

use std::{collections::BTreeMap, path::Path};

use crate::{
    data::{
        ARTIFACT_SCHEMA_VERSION, ArtifactCounts, ArtifactManifest, ArtifactRevisions, BuildOutput,
        DataError, static_transfers::enrich_network_with_static_transfers,
        write_bikeshare_artifact,
    },
    domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork},
    providers::bindings::TdxProviderBindings,
    street::StreetRouter,
};

#[derive(Clone, Debug)]
pub struct GtfsBuildOutput {
    pub build: BuildOutput,
    pub diagnostics: GtfsBuildDiagnostics,
}

/// Read, normalize, enrich, validate, and persist one local GTFS archive.
pub async fn build_from_gtfs_archive_with_street(
    input: &Path,
    output: &Path,
    street: &dyn StreetRouter,
) -> Result<GtfsBuildOutput, DataError> {
    let raw = archive::read(input)?;
    let (mut network, bindings, mut diagnostics) = normalize::normalize(raw);
    diagnostics.static_transfers =
        Some(enrich_network_with_static_transfers(&mut network, street).await?);
    persist_gtfs(input, output, network, bindings, diagnostics)
}

fn persist_gtfs(
    input: &Path,
    output: &Path,
    network: TransitNetwork,
    provider_bindings: TdxProviderBindings,
    diagnostics: GtfsBuildDiagnostics,
) -> Result<GtfsBuildOutput, DataError> {
    let bike_share_network = BikeShareNetwork::new(Vec::new());
    let static_revision = super::revision_for_network(&network, false)?;
    let schedule_revision = super::revision_for_network(&network, true)?;
    let network = network.with_revisions(static_revision.clone(), schedule_revision.clone());
    network.validate()?;
    provider_bindings
        .validate(&network)
        .map_err(DataError::InvalidProviderBindings)?;
    let bike_share_revision = super::revision_for_bike_share_network(&bike_share_network)?;
    let bike_share_network = bike_share_network.with_revision(bike_share_revision.clone());
    bike_share_network.validate()?;
    let source_hash = reader::hash_file(input)?;
    let manifest = ArtifactManifest {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        scope: "taiwan-gtfs".into(),
        built_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into()),
        raw_snapshot_date: None,
        sources: BTreeMap::from([(
            input
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("gtfs.zip")
                .to_owned(),
            source_hash,
        )]),
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
    super::write_network_artifact(output, &network)?;
    super::write_json_atomic(&output.join("provider-bindings.json"), &provider_bindings)?;
    write_bikeshare_artifact(output, &bike_share_network)?;
    super::write_json_atomic(&output.join("manifest.json"), &manifest)?;
    Ok(GtfsBuildOutput {
        build: BuildOutput {
            network,
            provider_bindings,
            bike_share_network,
            manifest,
            diagnostics: Default::default(),
        },
        diagnostics,
    })
}

#[cfg(test)]
mod tests;
