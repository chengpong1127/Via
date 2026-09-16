use std::{fs, path::PathBuf, process::Command};

use via_server::{
    data::{
        DataScope, build_from_raw, build_from_raw_with_transfer_preprocessing, load_manifest,
        load_network_artifact, load_transfer_artifact,
    },
    domain::transit::{TransitNetwork, TransitService, TransitStop},
    routing::RoutingLimits,
    street::StraightLineStreetRouter,
};

fn temporary_output(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("via-{name}-{}", uuid::Uuid::new_v4().simple()))
}

#[test]
fn fixture_build_is_offline_deterministic_and_secret_free() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/raw");
    let first = temporary_output("artifact-a");
    let second = temporary_output("artifact-b");
    let first_result = build_from_raw(
        &input,
        &first,
        Some(DataScope::TaipeiNewTaipei),
        Some("2026-09-15".into()),
    )
    .unwrap();
    let second_result = build_from_raw(
        &input,
        &second,
        Some(DataScope::TaipeiNewTaipei),
        Some("2026-09-15".into()),
    )
    .unwrap();

    assert_eq!(first_result.network, second_result.network);
    assert_eq!(
        first_result.manifest.revisions,
        second_result.manifest.revisions
    );
    assert_eq!(first_result.manifest.counts.stops, 10);
    assert_eq!(first_result.manifest.counts.services, 6);
    assert_eq!(first_result.manifest.counts.scheduled_trips, 3);
    assert_eq!(first_result.manifest.counts.bike_stations, 2);
    assert_eq!(first_result.manifest.counts.transfer_shortcuts, 1);

    let manifest = fs::read_to_string(first.join("manifest.json")).unwrap();
    assert!(!manifest.contains("TDX_CLIENT_SECRET"));
    assert!(!manifest.contains("access_token"));
    assert_eq!(
        load_manifest(&first.join("manifest.json"))
            .unwrap()
            .schema_version,
        1
    );
    assert_eq!(
        load_network_artifact(&first.join("transit-network.json")).unwrap(),
        first_result.network
    );
}

#[test]
fn invalid_network_shapes_fail_validation() {
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![TransitStop::new(
            via_server::domain::ids::BoardingPointId::new("stop"),
            "stop",
            via_server::api::RequestCoordinate {
                latitude: 25.0,
                longitude: 121.0,
            },
        )],
        vec![TransitService::new(
            via_server::domain::ids::TransitServiceId::new("service"),
            via_server::domain::transit::TransitMode::Bus,
            "service",
            "stop",
            vec![via_server::domain::ids::BoardingPointId::new("stop")],
        )],
        Vec::new(),
        Vec::new(),
    )
    .with_revisions("static", "schedule");
    assert!(network.validate().is_err());
}

#[tokio::test]
async fn explicit_transfer_preprocessing_writes_the_generic_artifact() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/raw");
    let output = temporary_output("preprocessed-transfers");
    let result = build_from_raw_with_transfer_preprocessing(
        &input,
        &output,
        Some(DataScope::TaipeiNewTaipei),
        Some("2026-09-15".into()),
        std::sync::Arc::new(StraightLineStreetRouter::default()),
        RoutingLimits::default(),
    )
    .await
    .unwrap();

    let transfer_index = load_transfer_artifact(&output.join("transfer-shortcuts.json")).unwrap();
    assert_eq!(
        transfer_index.len(),
        result.manifest.counts.transfer_shortcuts
    );
    assert_eq!(transfer_index.len(), 1);
    assert_eq!(
        result.manifest.transfer_config.max_bike_distance_metres,
        20_000
    );
}

#[test]
fn offline_commands_do_not_require_tdx_credentials() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/raw");
    let output = temporary_output("offline");
    let result = build_from_raw(&input, &output, None, None);
    assert!(result.is_ok());
}

#[test]
fn fixture_route_uses_the_production_routing_boundary() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/raw");
    let output = temporary_output("route");
    build_from_raw(&input, &output, Some(DataScope::TaipeiNewTaipei), None).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_via-route"))
        .args([
            "--network",
            output.join("transit-network.json").to_str().unwrap(),
            "--from-lat",
            "25.0478",
            "--from-lon",
            "121.5170",
            "--to-lat",
            "25.0339",
            "--to-lon",
            "121.5645",
            "--depart-at",
            "2026-09-15T07:55:00+08:00",
            "--modes",
            "transit",
            "--realtime",
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
