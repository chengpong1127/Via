//! Read the local GTFS feed and translate its transit data into Via types.

mod bindings;
mod normalize;
mod segment_profile;
#[cfg(test)]
mod tests;

use std::{collections::BTreeMap, path::Path};

use gtfs_structures::RawGtfs;
use serde::Serialize;

use crate::{
    data::DataError, domain::transit::TransitNetwork, providers::bindings::TdxProviderBindings,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GtfsBuildDiagnostics {
    pub stops_loaded: usize,
    pub routes_loaded_by_mode: BTreeMap<String, usize>,
    pub patterns_loaded_by_mode: BTreeMap<String, usize>,
    pub trips_loaded: usize,
    pub scheduled_trips_loaded: usize,
    pub frequencies_loaded: usize,
    pub shape_rows_loaded: usize,
    pub invalid_timing_trips: usize,
    pub invalid_timing_trip_examples: Vec<String>,
    pub exact_static_timing_patterns: usize,
    pub derived_bus_timing_patterns: usize,
    pub geometric_fallback_patterns: usize,
    pub unresolved_bus_realtime_scope_patterns: usize,
    pub unresolved_bus_realtime_scope_examples: Vec<String>,
    pub gtfs_stop_ids_without_tdx_mapping: usize,
    pub unmapped_stop_id_examples: Vec<String>,
    pub gtfs_pattern_ids_without_tdx_mapping: usize,
    pub unmapped_pattern_id_examples: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct GtfsBuildOutput {
    pub build: super::BuildOutput,
    pub diagnostics: GtfsBuildDiagnostics,
}

/// Parse the local feed, normalize its transit data, and attach TDX identities.
pub(super) fn build_network(
    input: &Path,
) -> Result<(TransitNetwork, TdxProviderBindings, GtfsBuildDiagnostics), DataError> {
    let raw = RawGtfs::from_path(input).map_err(|error| invalid_gtfs(input, error))?;
    let mut diagnostics = GtfsBuildDiagnostics::default();
    let normalized = normalize::normalize(raw, input, &mut diagnostics)?;
    let provider_bindings = bindings::build(
        &normalized.binding_inputs,
        &normalized.network,
        &mut diagnostics,
    );
    Ok((
        normalized.network.canonicalize(),
        provider_bindings,
        diagnostics,
    ))
}

fn invalid_gtfs(input: &Path, detail: impl std::fmt::Display) -> DataError {
    DataError::InvalidGtfsArchive {
        path: input.into(),
        detail: detail.to_string(),
    }
}
