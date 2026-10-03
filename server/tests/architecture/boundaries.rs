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
    let evaluator = source_tree("src/routing/evaluator");
    assert!(evaluator.contains("&TimingSnapshot"));
    assert!(evaluator.contains("SurfaceMovementPlanner"));
    assert!(evaluator.contains("optimistic_duration"));
    assert!(evaluator.contains(".realize(&request)"));
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
