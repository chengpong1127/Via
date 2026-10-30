use std::{fs, path::PathBuf};

fn source(path: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|error| {
        panic!("failed to read {path}: {error}");
    })
}

fn source_tree(path: &str) -> String {
    source_tree_with_test_files(path, false)
}

fn source_tree_with_test_files(path: &str, include_test_files: bool) -> String {
    fn collect(root: &std::path::Path, include_tests: bool) -> String {
        let mut paths = fs::read_dir(root)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
            .map(|entry| entry.expect("source entry").path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name().unwrap().to_str().unwrap();
                if !include_tests
                    && (name == "tests" || name == "tests.rs" || name.ends_with("_tests.rs"))
                {
                    return None;
                }
                if path.is_dir() {
                    Some(collect(&path, include_tests))
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    let text = fs::read_to_string(&path).expect("source file should be readable");
                    Some(if include_tests {
                        text
                    } else {
                        production_source(&text)
                    })
                } else {
                    None
                }
            })
            .collect()
    }
    collect(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path),
        include_test_files,
    )
}

fn production_source(text: &str) -> String {
    let mut output = String::new();
    let mut test_indent = None;
    for line in text.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if trimmed == "#[cfg(test)]" && test_indent.is_none() {
            test_indent = Some(indent);
            continue;
        }
        if let Some(start_indent) = test_indent {
            if indent == start_indent
                && !trimmed.starts_with("#[")
                && (trimmed.ends_with(';') || trimmed.ends_with(',') || trimmed.ends_with('}'))
            {
                test_indent = None;
            }
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

#[test]
fn domain_is_provider_and_transport_neutral() {
    let domain = source_tree("src/domain");
    for dependency in [
        "crate::api",
        "crate::providers",
        "crate::routing",
        "crate::street",
    ] {
        assert!(
            !domain.contains(dependency),
            "domain must not depend on {dependency}"
        );
    }
}

#[test]
fn routing_depends_on_domain_interfaces_not_transport_or_providers() {
    let routing = source_tree("src/routing");
    for dependency in ["crate::api", "api::", "crate::providers", "providers::"] {
        assert!(
            !routing.contains(dependency),
            "routing must not depend on transport or provider module {dependency}"
        );
    }
}

#[test]
fn reconstruction_does_not_depend_on_the_routing_service() {
    let reconstruction = source_tree("src/routing/reconstruct");
    assert!(
        !reconstruction.contains("RoutingService"),
        "reconstruction must not depend on RoutingService"
    );
}

#[test]
fn routing_does_not_access_multimodal_planner_topology() {
    let routing = source("src/routing/service.rs");
    for forbidden in ["TransitTopologyIndex", ".topology()", "AccessNetwork"] {
        assert!(
            !routing.contains(forbidden),
            "RoutingService must not access planner internals through {forbidden}"
        );
    }
}

#[test]
fn phase1_does_not_depend_on_realtime_or_bikeshare() {
    let static_search = source_tree("src/routing/static_search");
    for forbidden in [
        "DepartureQuery",
        "EffectiveTrip",
        "TimingSnapshot",
        "OffsetDateTime",
        "bikeshare",
        "BikeAvailability",
        "BikeStation",
        "StreetRouter",
        "SurfaceMovementPlanner",
        "StreetTravelMode::Bicycle",
        "scheduled_trip",
        "TDX",
        "reqwest",
    ] {
        assert!(
            !static_search.contains(forbidden),
            "Phase 1 static search contains realtime or BikeShare detail: {forbidden}"
        );
    }
}

#[test]
fn phase2_evaluator_uses_snapshots_and_surface_capabilities() {
    let evaluator = source_tree_with_test_files("src/routing/evaluator", false);
    for forbidden in [
        "TDX",
        "TdxClient",
        "reqwest",
        "get_json",
        "crate::providers",
        "TimingProvider",
        "BikeAvailabilitySource",
        "usable_pickups",
        "usable_dropoffs",
        "select_station",
        "static_search::generate",
        "best_baseline_arrival",
    ] {
        assert!(
            !evaluator.contains(forbidden),
            "Phase 2 evaluator bypasses its request snapshot or fixed-corridor boundary: {forbidden}"
        );
    }
}

#[test]
fn tdx_client_remains_transport_agnostic() {
    let client = source("src/tdx.rs");
    for forbidden in [
        "TransitMode",
        "ArrivalEstimate",
        "EffectiveTrip",
        "TdxBusEstimatedArrival",
        "Metro",
        "TRA",
        "THSR",
        "YouBike",
    ] {
        assert!(
            !client.contains(forbidden),
            "TdxClient knows about {forbidden}"
        );
    }
    assert!(client.contains("pub async fn get_json<T: DeserializeOwned>"));
}

#[test]
fn realized_journey_is_the_only_realized_path_representation() {
    let realized = source_tree("src/routing/realized");
    assert!(realized.contains("pub struct RealizedJourney {"));
    for legacy_declaration in [
        "pub struct JourneyVariant {",
        "pub struct JourneySkeleton {",
        "pub struct JourneyPathStep {",
        "pub enum JourneyPathMovement {",
    ] {
        assert!(
            !realized.contains(legacy_declaration),
            "routing must not retain a parallel realized path type: {legacy_declaration}"
        );
    }
}

#[test]
fn production_dependency_scan_excludes_test_items_and_keeps_following_production() {
    let source = "use crate::domain;\n#[cfg(test)]\nmod tests {\n    use crate::api;\n}\nuse crate::routing;\n    #[cfg(test)]\n    fn fixture() {\n        crate::providers::fixture();\n    }\n    fn production() {}\n";
    let production = production_source(source);
    assert!(!production.contains("crate::api"));
    assert!(!production.contains("crate::providers"));
    assert!(production.contains("crate::domain"));
    assert!(production.contains("crate::routing"));
    assert!(production.contains("fn production"));
}
