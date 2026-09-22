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
fn routing_has_one_service_entry_point_and_no_legacy_imports() {
    let service = source("src/routing/service.rs");
    assert!(service.contains("pub struct RoutingService"));
    assert!(service.contains("pub async fn route"));

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
fn canonical_domain_types_and_concrete_transit_planner_have_single_definitions() {
    let journey = source("src/domain/journey.rs");
    assert_eq!(journey.matches("enum JourneyMode").count(), 1);
    assert!(journey.contains("pub enum JourneyMode"));
    assert!(source("src/domain/ids.rs").contains("opaque_id!(TransitServiceId)"));
    let planner = source("src/routing/planner.rs");
    assert!(planner.contains("pub struct TransitPlanner"));
    assert!(planner.contains("DestinationReachability::build"));
    assert!(planner.contains("load_snapshot"));
    assert!(planner.contains("temporal::search"));
    assert!(
        !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/routing/algorithm.rs")
            .exists()
    );
    assert!(
        !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/routing/trip_based")
            .exists()
    );
}

#[test]
fn static_transfer_index_supports_both_search_directions_without_a_resolver() {
    let transfers = source("src/routing/transfers/index.rs");
    assert!(transfers.contains("by_from"));
    assert!(transfers.contains("by_to"));
    assert!(transfers.contains("transfers_from"));
    assert!(transfers.contains("transfers_to"));
    let routing = source_tree("src/routing");
    for obsolete in ["TransferResolver", "TransferNeed", "ResolvedTransfer"] {
        assert!(
            !routing.contains(obsolete),
            "obsolete transfer type remains: {obsolete}"
        );
    }
}

#[test]
fn structural_reachability_has_no_timing_dependency() {
    let reachability = source("src/routing/reachability.rs");
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
            !reachability.contains(forbidden),
            "structural reachability contains timing detail: {forbidden}"
        );
    }
}

#[test]
fn temporal_search_is_local_and_provider_free() {
    let temporal = source("src/routing/temporal.rs");
    assert!(temporal.contains("&TimingSnapshot"));
    assert!(temporal.contains("reachability.remaining_legs"));
    for forbidden in [
        "TdxClient",
        "reqwest",
        "get_json",
        ".await",
        "crate::providers",
    ] {
        assert!(
            !temporal.contains(forbidden),
            "temporal search contains I/O: {forbidden}"
        );
    }
}

#[test]
fn bus_realtime_requests_are_stop_oriented() {
    let bus = source("src/providers/timing/bus.rs");
    assert!(bus.contains("MAX_REALTIME_STOPS_PER_REQUEST"));
    assert!(bus.contains("odata_in_filter(\"StopUID\""));
    assert!(!bus.contains("odata_in_filter(\"SubRouteUID\""));
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
    assert!(service.contains("self.transit_planner.plan_with_stats(query).await"));
    assert!(service.contains("discover_access_points"));
    assert!(service.contains("discover_egress_points"));
}
