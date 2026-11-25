use std::collections::BTreeMap;

use via_server::data::{BuildDiagnostics, GtfsModeCoverage, ViaStatic};

pub(crate) fn print_static_counts(prefix: &str, via_static: &ViaStatic) {
    println!("{prefix} ViaStatic");
    println!("  schema version: {}", via_static.schema_version);
    println!(
        "  transit revision: {}",
        via_static.revisions.transit_static
    );
    println!("  schedule revision: {}", via_static.revisions.schedule);
    println!("  BikeShare revision: {}", via_static.revisions.bike_share);
    println!(
        "  stops={} routes={} patterns={} trips={} calendars={} frequencies={} transfers={} bike_stations={} bike_access_links={}",
        via_static.counts.stops,
        via_static.counts.routes,
        via_static.counts.patterns,
        via_static.counts.trips,
        via_static.counts.calendars,
        via_static.counts.frequencies,
        via_static.counts.transfers,
        via_static.counts.bike_stations,
        via_static.counts.bike_access_links
    );
}

pub(crate) fn print_build_diagnostics(diagnostics: &BuildDiagnostics) {
    let gtfs = &diagnostics.gtfs;
    if let Some(stats) = &diagnostics.static_transfers {
        println!(
            "walking transfers: boarding_points={} candidates={} matrix_pairs={} reachable={} retained={} removed_by_limits={} removed_by_fanout={} latency_ms={}",
            stats.boarding_points,
            stats.geographic_candidate_pairs,
            stats.matrix_pairs_queried,
            stats.reachable_walking_pairs,
            stats.retained_edges,
            stats.removed_by_limits,
            stats.removed_by_fanout,
            stats.elapsed_milliseconds
        );
    }
    if let Some(stats) = &diagnostics.bike_access {
        println!(
            "BikeShare access: boarding_points={} candidates={} walking_matrices={} access_links={} latency_ms={}",
            stats.boarding_points_processed,
            stats.geometric_candidate_pairs,
            stats.walking_matrix_requests,
            stats.retained_access_links,
            stats.build_wall_clock_milliseconds
        );
    }
    println!(
        "bus bindings with a TDX parent route: {}",
        diagnostics.bus_parent_routes
    );
    println!(
        "bus bindings with headway estimates: {}",
        diagnostics.bus_estimate_headways
    );
    println!("GTFS stops loaded: {}", gtfs.stops_loaded);
    println!("GTFS trips loaded: {}", gtfs.trips_loaded);
    println!(
        "exact scheduled trips loaded: {}",
        gtfs.scheduled_trips_loaded
    );
    println!("GTFS frequencies loaded: {}", gtfs.frequencies_loaded);
    println!("GTFS shape rows loaded: {}", gtfs.shape_rows_loaded);
    println!(
        "GTFS trips skipped for invalid exact timing: {}",
        gtfs.invalid_timing_trips
    );
    println!(
        "GTFS valid stops with no retained pattern: {}",
        gtfs.valid_unreferenced_stops
    );
    print_warning(
        "valid GTFS stop points are not referenced by any retained pattern",
        gtfs.valid_unreferenced_stops,
        &gtfs.unreferenced_stop_examples,
    );
    print_mode_coverage(&gtfs.coverage_by_mode);
    println!(
        "Bus Patterns without a TDX realtime scope: {}",
        gtfs.unresolved_bus_realtime_scope_patterns
    );
    println!(
        "GTFS stop IDs without current TDX ID mapping: {}",
        gtfs.gtfs_stop_ids_without_tdx_mapping
    );
    println!(
        "Via Pattern IDs without current TDX realtime mapping: {}",
        gtfs.gtfs_pattern_ids_without_tdx_mapping
    );
}

pub(crate) fn print_mode_coverage(coverage_by_mode: &BTreeMap<String, GtfsModeCoverage>) {
    for line in mode_coverage_lines(coverage_by_mode) {
        println!("{line}");
    }
}

fn mode_coverage_lines(coverage_by_mode: &BTreeMap<String, GtfsModeCoverage>) -> Vec<String> {
    let mut lines = Vec::new();
    for mode in ["bus", "metro", "rail", "high_speed_rail"] {
        let coverage = coverage_by_mode.get(mode).cloned().unwrap_or_default();
        lines.push(format!(
            "{mode} coverage: routes={} routes_without_patterns={} patterns={} gtfs_service={} without_gtfs_service={} live_provider_bound={} without_usable_service={} exact_timing={} derived_bus_timing={} estimated_or_mixed_timing={}",
            coverage.supported_routes,
            coverage.routes_without_patterns,
            coverage.patterns,
            coverage.patterns_with_gtfs_service,
            coverage.patterns_without_gtfs_service,
            coverage.patterns_with_live_provider_binding,
            coverage.patterns_without_usable_service,
            coverage.exact_static_timing_patterns,
            coverage.derived_bus_timing_patterns,
            coverage.estimated_or_mixed_timing_patterns,
        ));
        lines.extend(warning_line(
            &format!("{mode} routes have no retained pattern"),
            coverage.routes_without_patterns,
            &coverage.route_without_pattern_examples,
        ));
        lines.extend(warning_line(
            &format!("{mode} patterns have no GTFS scheduled trips or frequencies"),
            coverage.patterns_without_gtfs_service,
            &coverage.patterns_without_gtfs_service_examples,
        ));
        lines.extend(warning_line(
            &format!("{mode} patterns have no usable static or realtime service"),
            coverage.patterns_without_usable_service,
            &coverage.patterns_without_usable_service_examples,
        ));
        lines.extend(warning_line(
            &format!("{mode} patterns use estimated or mixed Phase 1 timing"),
            coverage.estimated_or_mixed_timing_patterns,
            &coverage.estimated_or_mixed_timing_examples,
        ));
    }
    lines
}

fn print_warning(label: &str, count: usize, examples: &[String]) {
    if let Some(line) = warning_line(label, count, examples) {
        println!("{line}");
    }
}

fn warning_line(label: &str, count: usize, examples: &[String]) -> Option<String> {
    (count > 0).then(|| {
        let examples = if examples.is_empty() {
            String::new()
        } else {
            format!("; examples: {}", examples.join(", "))
        };
        format!("WARNING: {label}: {count}{examples}")
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::mode_coverage_lines;
    use via_server::data::GtfsModeCoverage;

    #[test]
    fn inspect_stats_mode_coverage_reports_counts_and_examples() {
        let coverage = GtfsModeCoverage {
            supported_routes: 3,
            routes_without_patterns: 1,
            route_without_pattern_examples: vec!["bus:missing".to_owned()],
            patterns: 2,
            patterns_with_gtfs_service: 1,
            patterns_without_gtfs_service: 1,
            patterns_without_gtfs_service_examples: vec!["bus:ghost".to_owned()],
            patterns_with_live_provider_binding: 1,
            patterns_without_usable_service: 1,
            patterns_without_usable_service_examples: vec!["bus:ghost".to_owned()],
            exact_static_timing_patterns: 1,
            derived_bus_timing_patterns: 0,
            estimated_or_mixed_timing_patterns: 1,
            estimated_or_mixed_timing_examples: vec!["bus:estimated".to_owned()],
        };
        let lines = mode_coverage_lines(&BTreeMap::from([("bus".to_owned(), coverage)]));

        assert!(lines.iter().any(|line| {
            line == "bus coverage: routes=3 routes_without_patterns=1 patterns=2 gtfs_service=1 without_gtfs_service=1 live_provider_bound=1 without_usable_service=1 exact_timing=1 derived_bus_timing=0 estimated_or_mixed_timing=1"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus routes have no retained pattern: 1; examples: bus:missing"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus patterns have no GTFS scheduled trips or frequencies: 1; examples: bus:ghost"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus patterns have no usable static or realtime service: 1; examples: bus:ghost"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus patterns use estimated or mixed Phase 1 timing: 1; examples: bus:estimated"
        }));
    }
}
