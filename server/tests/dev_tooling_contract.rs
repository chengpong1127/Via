use std::{fs, path::PathBuf, process::Command};

use via_server::{
    data::{
        DataScope, StaticTransferPolicy, build_from_raw, build_from_raw_with_street, load_manifest,
        load_network_artifact,
    },
    domain::transit::{TransitNetwork, TransitService, TransitStop},
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
    assert_eq!(first_result.manifest.counts.stops, 11);
    assert_eq!(first_result.manifest.counts.services, 6);
    assert_eq!(first_result.manifest.counts.scheduled_trips, 3);
    assert_eq!(first_result.manifest.counts.transfer_edges, 2);
    assert_eq!(first_result.network.transfer_edges.len(), 2);
    assert!(
        first_result.network.transfer_seconds(
            &via_server::domain::ids::BoardingPointId::new("TRTC-BL12"),
            &via_server::domain::ids::BoardingPointId::new("TRTC-R10"),
        ) == Some(240)
    );

    let manifest = fs::read_to_string(first.join("manifest.json")).unwrap();
    assert!(!manifest.contains("TDX_CLIENT_SECRET"));
    assert!(!manifest.contains("access_token"));
    assert_eq!(
        load_manifest(&first.join("manifest.json"))
            .unwrap()
            .schema_version,
        4
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
    )
    .with_revisions("static", "schedule");
    assert!(network.validate().is_err());
}

#[test]
fn build_writes_transfers_only_in_the_network_artifact() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/raw");
    let output = temporary_output("preprocessed-transfers");
    let result = build_from_raw(
        &input,
        &output,
        Some(DataScope::TaipeiNewTaipei),
        Some("2026-09-15".into()),
    )
    .unwrap();

    assert_eq!(result.network.transfer_edges.len(), 2);
    assert!(!output.join("transfer-shortcuts.json").exists());
    assert_eq!(result.manifest.counts.transfer_edges, 2);
}

#[tokio::test]
async fn enriched_build_publishes_walking_transfers_in_the_network_artifact() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/raw");
    let output = temporary_output("enriched-artifact");
    let street = StraightLineStreetRouter::default();
    let result = build_from_raw_with_street(
        &input,
        &output,
        Some(DataScope::TaipeiNewTaipei),
        Some("2026-09-15".into()),
        &street,
        StaticTransferPolicy::default(),
    )
    .await
    .unwrap();

    let discovery = result.diagnostics.transfer_discovery.unwrap();
    assert_eq!(discovery.boarding_points, result.network.stops.len());
    assert_eq!(
        discovery.source_points_processed,
        result.network.stops.len()
    );
    assert_eq!(
        discovery.total_transfer_edges,
        result.network.transfer_edges.len()
    );
    assert!(discovery.inferred_edges_kept > 0);
    assert!(!output.join("transfer-shortcuts.json").exists());
    assert_eq!(
        load_network_artifact(&output.join("transit-network.json")).unwrap(),
        result.network
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
