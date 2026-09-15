use std::{fs, path::PathBuf};

fn source(path: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|error| {
        panic!("failed to read {path}: {error}");
    })
}

fn source_tree(path: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    let mut files = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    files.sort();
    files
        .into_iter()
        .map(|path| fs::read_to_string(path).expect("source file should be readable"))
        .collect()
}

#[test]
fn legacy_architecture_modules_are_gone() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for path in [
        "planner.rs",
        "normalization.rs",
        "ranking.rs",
        "tdx.rs",
        "transit.rs",
        "live_status.rs",
        "routing/engine",
    ] {
        assert!(
            !root.join(path).exists(),
            "legacy path still exists: {path}"
        );
    }
}

#[test]
fn database_identity_infrastructure_is_gone() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "src/entity.rs",
        "src/migration.rs",
        "src/providers/postgres.rs",
        "tests/postgres_identity_contract.rs",
    ] {
        assert!(
            !root.join(path).exists(),
            "obsolete path still exists: {path}"
        );
    }
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
fn routing_uses_provider_traits_instead_of_concrete_adapters() {
    let routing = source_tree("src/routing");
    for dependency in [
        "crate::providers::tdx",
        "crate::providers::valhalla",
        "crate::providers::postgres",
    ] {
        assert!(
            !routing.contains(dependency),
            "routing must not depend on concrete adapter {dependency}"
        );
    }
}

#[test]
fn routing_has_one_service_entry_point_and_no_legacy_imports() {
    let service = source("src/routing/service.rs");
    assert!(service.contains("pub struct RoutingService"));
    assert!(service.contains("pub async fn route"));

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for path in [
        "planner.rs",
        "normalization.rs",
        "routing/engine.rs",
        "routing/engine/mod.rs",
    ] {
        assert!(
            !root.join(path).exists(),
            "legacy path still exists: {path}"
        );
    }
    for source in [
        source("src/routing/service.rs"),
        source("src/routing/reconstruct/mod.rs"),
    ] {
        for legacy_import in [
            "crate::planner",
            "crate::normalization",
            "crate::routing::engine",
            "JourneyPlanningService",
        ] {
            assert!(
                !source.contains(legacy_import),
                "routing source contains legacy import or wrapper: {legacy_import}"
            );
        }
    }
}

#[test]
fn reconstruction_modules_have_explicit_responsibilities() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routing/reconstruct");
    for file in ["mod.rs", "legs.rs", "geometry.rs", "evidence.rs"] {
        assert!(
            root.join(file).is_file(),
            "missing reconstruction module: {file}"
        );
    }
    assert!(source("src/routing/reconstruct/mod.rs").contains("legs::assemble"));
    assert!(source("src/routing/reconstruct/legs.rs").contains("append_transfer_path"));
    assert!(source("src/routing/reconstruct/legs.rs").contains("hydrate_path"));
    assert!(source("src/routing/reconstruct/evidence.rs").contains("summary_from_evidence"));
}

#[test]
fn canonical_domain_types_and_algorithm_boundary_have_single_definitions() {
    let journey = source("src/domain/journey.rs");
    assert_eq!(journey.matches("enum JourneyMode").count(), 1);
    assert!(journey.contains("pub enum JourneyMode"));
    assert!(source("src/domain/ids.rs").contains("opaque_id!(TransitServiceId)"));
    assert!(source("src/routing/algorithm.rs").contains("pub trait RoutingAlgorithm"));
    assert!(
        source("src/routing/trip_based.rs").contains("impl RoutingAlgorithm for TripBasedRouter")
    );
}

#[test]
fn routing_has_no_legacy_transit_pipeline() {
    let service = source("src/routing/service.rs");
    let routing = source_tree("src/routing");
    for legacy in [
        "DiscoveryResult",
        "RouteGraph",
        "BikeShareOpportunityDiscovery",
        "TransitTimingProvider",
        "collect_request_realtime",
        "search::search",
        "add_timed_transit",
        "add_bike_alternatives",
    ] {
        assert!(
            !routing.contains(legacy),
            "legacy routing symbol remains: {legacy}"
        );
    }
    assert!(service.contains("self.routing_algorithm.route(query).await"));
    assert!(service.contains("discover_access_points"));
    assert!(service.contains("discover_egress_points"));
}

#[test]
fn obsolete_routing_files_are_deleted() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routing");
    for file in [
        "discover.rs",
        "enrich.rs",
        "bike.rs",
        "graph.rs",
        "search.rs",
    ] {
        assert!(
            !root.join(file).exists(),
            "obsolete routing file remains: {file}"
        );
    }
}
