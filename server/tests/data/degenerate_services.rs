use std::path::PathBuf;

use via_server::data::build_from_raw;

fn temporary_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "via-degenerate-service-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/tdx/test-data")
        .join(name)
}

#[test]
fn degenerate_bus_services_are_skipped_without_collapsing_valid_or_loop_patterns() {
    let input = fixture_path("degenerate-services");
    let output = temporary_path("artifact");

    let result = build_from_raw(&input, &output, None, None).unwrap();

    assert_eq!(result.diagnostics.skipped_services.len(), 1);
    assert_eq!(
        result.diagnostics.skipped_services[0].service_id,
        "ONE::direction=0"
    );
    assert_eq!(
        result.diagnostics.skipped_services[0].reason,
        "pattern_too_short"
    );
    assert_eq!(result.diagnostics.skipped_services[0].stops, 1);
    assert_eq!(result.diagnostics.duplicate_bus_stop_artifacts_removed, 1);
    assert!(
        !result
            .network
            .services
            .iter()
            .any(|service| service.id.as_str() == "ONE::direction=0")
    );
    assert!(
        !result
            .network
            .trips
            .iter()
            .any(|trip| trip.service_id.as_str() == "ONE::direction=0")
    );

    let valid = result
        .network
        .services
        .iter()
        .find(|service| service.id.as_str() == "VALID::direction=0")
        .unwrap();
    assert_eq!(
        valid
            .pattern
            .iter()
            .map(|stop| stop.as_str())
            .collect::<Vec<_>>(),
        ["VALID-A", "VALID-B"]
    );

    let loop_service = result
        .network
        .services
        .iter()
        .find(|service| service.id.as_str() == "LOOP::direction=0")
        .unwrap();
    assert_eq!(
        loop_service
            .pattern
            .iter()
            .map(|stop| stop.as_str())
            .collect::<Vec<_>>(),
        ["LOOP-A", "LOOP-B", "LOOP-A"]
    );
    let duplicate_service = result
        .network
        .services
        .iter()
        .find(|service| service.id.as_str() == "DUP::direction=0")
        .unwrap();
    assert_eq!(
        duplicate_service
            .pattern
            .iter()
            .map(|stop| stop.as_str())
            .collect::<Vec<_>>(),
        ["DUP-A", "DUP-B"]
    );
    result.network.validate().unwrap();
}

#[test]
fn widespread_bus_stop_of_route_corruption_is_reported_as_a_schema_mismatch() {
    let input = fixture_path("systemic-bus-corruption");
    let output = temporary_path("systemic-artifact");

    let error = build_from_raw(&input, &output, None, None).unwrap_err();

    assert!(error.to_string().contains("systemic schema mismatch"));
}
