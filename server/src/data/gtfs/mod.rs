mod archive;
mod bindings;
mod normalize;
mod patterns;
mod segment_profile;
#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

use serde::Serialize;

use crate::{
    data::DataError,
    domain::{
        ids::{TransitPatternId, TransitTripId},
        transit::{SegmentTimeSource, TransitMode, TransitNetwork},
    },
    providers::tdx_bindings::TdxProviderBindings,
};

pub const MAX_DIAGNOSTIC_EXAMPLES: usize = 10;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GtfsModeCoverage {
    pub supported_routes: usize,
    pub routes_without_patterns: usize,
    pub route_without_pattern_examples: Vec<String>,
    pub patterns: usize,
    pub patterns_with_gtfs_service: usize,
    pub patterns_without_gtfs_service: usize,
    pub patterns_without_gtfs_service_examples: Vec<String>,
    pub patterns_with_live_provider_binding: usize,
    pub patterns_without_usable_service: usize,
    pub patterns_without_usable_service_examples: Vec<String>,
    pub exact_static_timing_patterns: usize,
    pub derived_bus_timing_patterns: usize,
    pub estimated_or_mixed_timing_patterns: usize,
    pub estimated_or_mixed_timing_examples: Vec<String>,
}

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
    pub valid_unreferenced_stops: usize,
    pub unreferenced_stop_examples: Vec<String>,
    pub coverage_by_mode: BTreeMap<String, GtfsModeCoverage>,
}

#[derive(Clone, Debug)]
pub struct GtfsBuildOutput {
    pub build: super::BuildOutput,
    pub diagnostics: GtfsBuildDiagnostics,
}

pub(super) fn build_network(
    input: &Path,
) -> Result<(TransitNetwork, TdxProviderBindings, GtfsBuildDiagnostics), DataError> {
    let raw = archive::read(input)?;
    let mut diagnostics = GtfsBuildDiagnostics::default();
    let normalized = normalize::normalize(raw, input, &mut diagnostics)?;
    let provider_bindings = bindings::build(
        &normalized.binding_inputs,
        &normalized.network,
        &mut diagnostics,
    );
    diagnostics.coverage_by_mode = measure_mode_coverage(&normalized.network, &provider_bindings);
    let pattern_count = diagnostics
        .coverage_by_mode
        .values()
        .map(|coverage| coverage.patterns)
        .sum::<usize>();
    let unserviceable_pattern_count = diagnostics
        .coverage_by_mode
        .values()
        .map(|coverage| coverage.patterns_without_usable_service)
        .sum::<usize>();
    if pattern_count == unserviceable_pattern_count {
        let supported_route_count = diagnostics
            .coverage_by_mode
            .values()
            .map(|coverage| coverage.supported_routes)
            .sum::<usize>();
        let route_examples = diagnostics
            .coverage_by_mode
            .values()
            .flat_map(|coverage| coverage.route_without_pattern_examples.iter())
            .take(MAX_DIAGNOSTIC_EXAMPLES)
            .cloned()
            .collect::<Vec<_>>();
        let details = format!(
            "supported_routes={supported_route_count} retained_patterns={pattern_count} patterns_without_usable_service={unserviceable_pattern_count} routes_without_patterns={route_examples:?}"
        );
        return Err(DataError::NoUsableRouteService {
            path: input.into(),
            details,
        });
    }
    Ok((
        normalized.network.canonicalize(),
        provider_bindings,
        diagnostics,
    ))
}

pub fn measure_mode_coverage(
    network: &TransitNetwork,
    provider_bindings: &TdxProviderBindings,
) -> BTreeMap<String, GtfsModeCoverage> {
    let routes = network
        .routes
        .iter()
        .map(|route| (route.id.clone(), route.mode))
        .collect::<HashMap<_, _>>();
    let routes_with_patterns = network
        .patterns
        .iter()
        .map(|pattern| pattern.route_id.clone())
        .collect::<HashSet<_>>();
    let mut coverage = BTreeMap::<String, GtfsModeCoverage>::new();
    for route in &network.routes {
        let mode_coverage = coverage
            .entry(mode_name(route.mode).to_owned())
            .or_default();
        mode_coverage.supported_routes += 1;
        if !routes_with_patterns.contains(&route.id) {
            mode_coverage.routes_without_patterns += 1;
            push_example(
                &mut mode_coverage.route_without_pattern_examples,
                route.id.as_str(),
            );
        }
    }

    let gtfs_service_patterns = network
        .trips
        .iter()
        .map(|trip| trip.pattern_id.clone())
        .chain(
            network
                .frequencies
                .iter()
                .map(|frequency| frequency.pattern_id.clone()),
        )
        .collect::<HashSet<TransitPatternId>>();
    let mut live_patterns = provider_bindings
        .bus_patterns
        .keys()
        .chain(provider_bindings.metro_patterns.keys())
        .cloned()
        .collect::<HashSet<_>>();
    let pattern_by_trip = network
        .trips
        .iter()
        .map(|trip| (trip.id.clone(), trip.pattern_id.clone()))
        .collect::<HashMap<TransitTripId, TransitPatternId>>();
    for trip_id in provider_bindings.tra_trips.keys() {
        if let Some(pattern_id) = pattern_by_trip.get(trip_id) {
            live_patterns.insert(pattern_id.clone());
        }
    }

    for pattern in &network.patterns {
        let Some(mode) = routes.get(&pattern.route_id).copied() else {
            continue;
        };
        let mode_coverage = coverage.entry(mode_name(mode).to_owned()).or_default();
        mode_coverage.patterns += 1;
        let has_gtfs_service = gtfs_service_patterns.contains(&pattern.id);
        if has_gtfs_service {
            mode_coverage.patterns_with_gtfs_service += 1;
        } else {
            mode_coverage.patterns_without_gtfs_service += 1;
            push_example(
                &mut mode_coverage.patterns_without_gtfs_service_examples,
                pattern.id.as_str(),
            );
        }
        if live_patterns.contains(&pattern.id) {
            mode_coverage.patterns_with_live_provider_binding += 1;
        } else if !has_gtfs_service {
            mode_coverage.patterns_without_usable_service += 1;
            push_example(
                &mut mode_coverage.patterns_without_usable_service_examples,
                pattern.id.as_str(),
            );
        }

        if pattern
            .static_profile
            .segments
            .iter()
            .all(|segment| segment.source == SegmentTimeSource::Schedule)
        {
            mode_coverage.exact_static_timing_patterns += 1;
        } else if pattern
            .static_profile
            .segments
            .iter()
            .all(|segment| segment.source == SegmentTimeSource::Derived)
        {
            mode_coverage.derived_bus_timing_patterns += 1;
        } else {
            mode_coverage.estimated_or_mixed_timing_patterns += 1;
            push_example(
                &mut mode_coverage.estimated_or_mixed_timing_examples,
                pattern.id.as_str(),
            );
        }
    }
    coverage
}

pub(super) fn push_example(examples: &mut Vec<String>, value: impl Into<String>) {
    if examples.len() < MAX_DIAGNOSTIC_EXAMPLES {
        examples.push(value.into());
    }
}

fn mode_name(mode: TransitMode) -> &'static str {
    match mode {
        TransitMode::Bus => "bus",
        TransitMode::Metro => "metro",
        TransitMode::Rail => "rail",
        TransitMode::HighSpeedRail => "high_speed_rail",
    }
}

fn invalid_gtfs(input: &Path, detail: impl std::fmt::Display) -> DataError {
    DataError::InvalidGtfsArchive {
        path: input.into(),
        detail: detail.to_string(),
    }
}
