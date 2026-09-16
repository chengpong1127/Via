use std::{fs, path::Path, path::PathBuf};

use serde_json::json;
use via_server::data::build_from_raw;

fn temporary_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "via-degenerate-service-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn write_json(path: &Path, value: serde_json::Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn stop(uid: &str, sequence: u64, latitude: f64, longitude: f64) -> serde_json::Value {
    json!({
        "StopUID": uid,
        "StopName": {"Zh_tw": uid},
        "StopSequence": sequence,
        "StopPosition": {"PositionLat": latitude, "PositionLon": longitude}
    })
}

#[test]
fn degenerate_bus_services_are_skipped_without_collapsing_valid_or_loop_patterns() {
    let input = temporary_path("raw");
    let output = temporary_path("artifact");

    write_json(
        &input.join("bus/stop.json"),
        json!([
            stop("ONE-A", 0, 25.0, 121.0),
            stop("ONE-B", 0, 25.0, 121.001),
            stop("VALID-A", 0, 25.02, 121.02),
            stop("VALID-B", 0, 25.01, 121.01),
            stop("LOOP-A", 0, 25.03, 121.03),
            stop("LOOP-B", 0, 25.04, 121.04),
            stop("DUP-A", 0, 25.05, 121.05),
            stop("DUP-B", 0, 25.06, 121.06)
        ]),
    );
    write_json(
        &input.join("bus/schedule.json"),
        json!([
            {
                "SubRouteUID": "ONE",
                "Direction": 0,
                "StopTimes": [
                    {"StopUID": "ONE-A", "ArrivalTime": "08:00", "DepartureTime": "08:00"},
                    {"StopUID": "ONE-B", "ArrivalTime": "08:10", "DepartureTime": "08:10"}
                ]
            }
        ]),
    );
    write_json(
        &input.join("bus/stop-of-route.json"),
        json!([
            {
                "SubRouteUID": "ONE",
                "Direction": 0,
                "Stops": [stop("ONE-A", 1, 25.0, 121.0)]
            },
            {
                "SubRouteUID": "VALID",
                "Direction": 0,
                "Stops": [
                    stop("VALID-B", 2, 25.01, 121.01),
                    stop("VALID-A", 1, 25.02, 121.02)
                ]
            },
            {
                "SubRouteUID": "LOOP",
                "Direction": 0,
                "Stops": [
                    stop("LOOP-A", 1, 25.03, 121.03),
                    stop("LOOP-B", 2, 25.04, 121.04),
                    stop("LOOP-A", 3, 25.03, 121.03)
                ]
            },
            {
                "SubRouteUID": "DUP",
                "Direction": 0,
                "Stops": [
                    stop("DUP-A", 1, 25.05, 121.05),
                    stop("DUP-A", 1, 25.05, 121.05),
                    stop("DUP-B", 2, 25.06, 121.06)
                ]
            }
        ]),
    );

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
    let input = temporary_path("systemic-raw");
    let output = temporary_path("systemic-artifact");
    write_json(
        &input.join("bus/stop-of-route.json"),
        json!([{"Stops": []}, {"Stops": []}]),
    );

    let error = build_from_raw(&input, &output, None, None).unwrap_err();

    assert!(error.to_string().contains("systemic schema mismatch"));
}
