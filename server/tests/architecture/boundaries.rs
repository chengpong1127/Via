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
    assert!(source("src/routing/reconstruct/legs.rs").contains("add_transfer"));
    let legs = source("src/routing/reconstruct/legs.rs");
    let transfer = legs
        .split_once("fn add_transfer")
        .map(|(_, rest)| rest)
        .expect("transfer reconstruction function should be present");
    assert!(transfer.contains("transfer_evidence"));
    assert!(!transfer.contains("hydrate_path"));
    assert!(!transfer.contains("service.street_path("));
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
        source("src/routing/trip_based/search.rs")
            .contains("impl RoutingAlgorithm for TripBasedRouter")
    );
}

#[test]
fn trip_based_search_uses_the_generic_transfer_boundary() {
    let search = source("src/routing/trip_based/search.rs");
    assert!(search.contains("transfer_resolver.expand_batch"));
    for forbidden in [
        "TransferShortcutIndex",
        "transfers_from",
        "WalkingTransfer",
        "BikeShare",
        "YouBike",
        "Valhalla",
    ] {
        assert!(
            !search.contains(forbidden),
            "Trip-Based search contains {forbidden}"
        );
    }
}

#[test]
fn trip_based_search_is_transport_timing_neutral() {
    let trip_based = source_tree("src/routing/trip_based");
    for forbidden in [
        "BusDepartureSource",
        "MetroDepartureSource",
        "TraDepartureSource",
        "ThsrDepartureSource",
        "ServiceConditionProvider",
        "TDX",
        "realtime_scope",
    ] {
        assert!(
            !trip_based.contains(forbidden),
            "Trip-Based routing contains transport timing detail: {forbidden}"
        );
    }
}

#[test]
fn departure_realtime_is_owned_by_the_mode_source() {
    let bus = source("src/providers/timing/bus.rs");
    assert!(bus.contains("TdxClient"));
    assert!(bus.contains("get_json"));
    for forbidden in ["RealtimeProvider", "ArrivalQuery", "BusRealtimeResolver"] {
        assert!(
            !bus.contains(forbidden),
            "bus departure timing still depends on {forbidden}"
        );
    }

    let service_conditions = source("src/providers/service_conditions/mod.rs");
    for forbidden in ["ArrivalQuery", "arrival_estimates", "arrival_pair_cache"] {
        assert!(
            !service_conditions.contains(forbidden),
            "service-condition layer still owns departure arrivals: {forbidden}"
        );
    }

    let tdx = source("src/providers/service_conditions/tdx.rs");
    for forbidden in ["ArrivalEstimate", "arrival_estimates", "TdxRealtimeConfig"] {
        assert!(
            !tdx.contains(forbidden),
            "status adapter still owns bus departure timing: {forbidden}"
        );
    }

    let app = source("src/app.rs");
    assert!(app.contains("BusDepartureSource::new"));
    assert!(!app.contains("BusRealtimeResolver"));
}

#[test]
fn tdx_client_remains_transport_agnostic() {
    let client = source("src/tdx/client.rs");
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
fn service_conditions_do_not_own_departure_timing() {
    let provider = source("src/providers/service_conditions/tdx.rs");
    for forbidden in ["DepartureQuery", "EffectiveTrip", "ArrivalEstimate"] {
        assert!(
            !provider.contains(forbidden),
            "service-condition provider contains departure timing detail: {forbidden}"
        );
    }
    assert!(provider.contains("Arc<TdxClient>"));
    assert!(provider.contains("impl ServiceConditionProvider"));
}

#[test]
fn metrics_have_separate_service_condition_and_bus_owners() {
    let service_conditions = source("src/providers/service_conditions/mod.rs");
    let bus = source("src/providers/timing/bus.rs");
    assert!(service_conditions.contains("ServiceConditionMetrics"));
    assert!(!service_conditions.contains("logical_departure_needs"));
    assert!(bus.contains("BusTimingMetrics"));
    assert!(!bus.contains("ServiceConditionMetrics"));
}

#[test]
fn routing_has_no_legacy_transit_pipeline() {
    let service = source("src/routing/service.rs");
    let routing = source_tree("src/routing");
    for legacy in [
        "DiscoveryResult",
        "RouteGraph",
        "TransitTimingProvider",
        "collect_request_realtime",
        "search::search",
        "add_timed_transit",
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
    for file in ["discover.rs", "enrich.rs", "graph.rs", "search.rs"] {
        assert!(
            !root.join(file).exists(),
            "obsolete routing file remains: {file}"
        );
    }
}
