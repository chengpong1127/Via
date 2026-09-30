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
fn phase1_does_not_depend_on_realtime_or_bikeshare() {
    let static_search = source("src/routing/static_search.rs");
    for forbidden in [
        "DepartureQuery",
        "EffectiveTrip",
        "TimingSnapshot",
        "OffsetDateTime",
        "bikeshare",
        "BikeAvailability",
        "BikeStation",
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
fn phase2_evaluator_uses_request_snapshots_and_lazy_bike_routing() {
    let evaluator = source("src/routing/evaluator.rs");
    assert!(evaluator.contains("&TimingSnapshot"));
    assert!(evaluator.contains("corridor.retained()"));
    assert!(evaluator.contains("BikeAvailabilitySnapshot"));
    assert!(evaluator.contains("movement_candidate"));
    assert!(evaluator.contains("realize_candidate"));
    for forbidden in [
        "TDX",
        "TdxClient",
        "reqwest",
        "get_json",
        "crate::providers",
        "TimingProvider",
        "BikeAvailabilitySource",
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
