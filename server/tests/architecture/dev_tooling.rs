use std::{path::PathBuf, process::Command};

#[test]
fn via_data_exposes_local_gtfs_build_without_a_fetch_command() {
    let help = Command::new(env!("CARGO_BIN_EXE_via-data"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("build"));
    assert!(help.contains("validate"));
    assert!(help.contains("inspect"));
    assert!(!help.contains("fetch"));

    let build_help = Command::new(env!("CARGO_BIN_EXE_via-data"))
        .args(["build", "--help"])
        .output()
        .unwrap();
    assert!(build_help.status.success());
    let build_help = String::from_utf8_lossy(&build_help.stdout);
    assert!(build_help.contains("Local GTFS .zip archive"));
    assert!(!build_help.contains("TDX"));
}

#[test]
fn fixture_route_uses_the_production_routing_boundary() {
    let network = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/routing/network.json");
    let result = Command::new(env!("CARGO_BIN_EXE_via-route"))
        .args([
            "--network",
            network.to_str().unwrap(),
            "--from-lat",
            "25.1000",
            "--from-lon",
            "121.5000",
            "--to-lat",
            "25.1030",
            "--to-lon",
            "121.5030",
            "--depart-at",
            "2026-08-31T10:00:00+08:00",
            "--modes",
            "bus,metro,tra,thsr",
            "--service-conditions",
            "fixture",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "via-route failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(json["journey_options"].as_array().is_some_and(|options| {
        options.iter().any(|option| {
            option["legs"]
                .as_array()
                .is_some_and(|legs| legs.iter().any(|leg| leg["details"]["type"] == "transit"))
        })
    }));
}
