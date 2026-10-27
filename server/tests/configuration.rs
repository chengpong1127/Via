use std::{path::Path, process::Command, time::Duration};

use via_server::config::{Environment, ViaConfig};

const TOML: &str = include_str!("../config/via.toml");

#[test]
fn tracked_configuration_preserves_routing_and_provider_defaults() {
    let settings = ViaConfig::from_toml(TOML).unwrap();
    let limits = settings.journey_limits();
    assert_eq!(limits.planner.max_transfers, 4);
    assert_eq!(limits.planner.max_static_corridors, 100);
    assert_eq!(limits.planner.max_phase1_expansions, 20_000);
    assert_eq!(limits.planner.max_candidates, 16);
    assert_eq!(limits.planner.max_departures_per_query, 3);
    assert_eq!(limits.planner.max_journey_duration_seconds, 14_400);
    assert_eq!(limits.request_timeout, Duration::from_secs(5));
    assert_eq!(
        settings.routing.ranking,
        via_server::domain::ranking::RankingPolicy::default()
    );
    assert_eq!(settings.tdx.batch.bike_stations, 400);
    assert_eq!(settings.tdx.batch.bus_stops, 200);
    assert_eq!(settings.tdx.batch.metro_pairs, 200);
    assert_eq!(
        settings
            .valhalla
            .provider_config(false)
            .unwrap()
            .request_timeout,
        Duration::from_millis(1200)
    );
    assert_eq!(
        settings
            .valhalla
            .provider_config(true)
            .unwrap()
            .request_timeout,
        Duration::from_secs(15)
    );
}

#[test]
fn rejects_malformed_unknown_negative_zero_and_overflowing_settings() {
    for body in [
        "this is not TOML".to_owned(),
        TOML.replace("port = 3000", "port = 3000\ntypo = 1"),
        TOML.replace("bus_stops = 200", "bus_stops = 0"),
        TOML.replace("metro_pairs = 200", "metro_pairs = -1"),
        TOML.replace("request_timeout_ms = 1200", "request_timeout_ms = 0"),
        TOML.replace(
            "request_timeout_ms = 1200",
            "request_timeout_ms = 9223372036854775807",
        ),
        TOML.replace("max_sources = 32", "max_sources = 0"),
        TOML.replace("degraded_seconds = 180", "degraded_seconds = 119"),
        TOML.replace("max_targets = 32", "max_targets = 8"),
        TOML.replace(
            "endpoint_radius_metres = 1500",
            "endpoint_radius_metres = 4294967296",
        ),
        TOML.replace("http://127.0.0.1:8002/", "not a URL"),
        TOML.replace("http://127.0.0.1:8002/", "file:///tmp/router"),
        TOML.replace("http://127.0.0.1:8002/", "https://user:secret@example.com/"),
        TOML.replace(
            "optimistic_walk_speed_metres_per_second = 2.0",
            "optimistic_walk_speed_metres_per_second = nan",
        ),
    ] {
        assert!(ViaConfig::from_toml(&body).is_err(), "accepted {body}");
    }
}

#[test]
fn explicit_load_failures_identify_the_path() {
    let path = Path::new("/missing-via-config-test.toml");
    assert!(
        ViaConfig::load(Some(path))
            .unwrap_err()
            .to_string()
            .contains(path.to_str().unwrap())
    );
    let path = std::env::temp_dir().join(format!("via-invalid-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&path, "[invalid").unwrap();
    let error = ViaConfig::load(Some(&path)).unwrap_err().to_string();
    std::fs::remove_file(&path).unwrap();
    assert!(error.contains(path.to_str().unwrap()));
}

#[test]
fn production_requires_https_for_api_token_and_matrix_urls() {
    let mut config = ViaConfig::default();
    assert!(
        config
            .validate_environment(Environment::Development)
            .is_ok()
    );
    assert!(
        config
            .validate_environment(Environment::Production)
            .is_err()
    );
    config.valhalla.base_url = "https://router.example.com/".into();
    assert!(config.validate_environment(Environment::Production).is_ok());
    config.tdx.token_url = "http://tokens.example.com/".into();
    assert!(
        config
            .validate_environment(Environment::Production)
            .is_err()
    );
    config.tdx.token_url = ViaConfig::default().tdx.token_url;
    config.tdx.api_base_url = "http://api.example.com/".into();
    assert!(
        config
            .validate_environment(Environment::Production)
            .is_err()
    );
}

#[test]
fn explicit_path_precedes_environment_and_default_is_absolute() {
    let explicit = Path::new("/explicit.toml");
    assert_eq!(
        ViaConfig::resolve_path(Some(explicit), Some("/env.toml".into())),
        explicit
    );
    assert_eq!(
        ViaConfig::resolve_path(None, Some("/env.toml".into())),
        Path::new("/env.toml")
    );
    assert!(ViaConfig::resolve_path(None, None).is_absolute());
}

#[test]
fn changed_toml_reaches_planning_matrix_and_preprocessing_limits() {
    let config = ViaConfig::from_toml(
        &TOML
            .replace("max_candidates = 16", "max_candidates = 7")
            .replace("max_sources = 32", "max_sources = 17")
            .replace(
                "max_geometric_radius_metres = 300",
                "max_geometric_radius_metres = 42",
            ),
    )
    .unwrap();
    assert_eq!(config.journey_limits().planner.max_candidates, 7);
    assert_eq!(
        config.valhalla.provider_config(false).unwrap().max_sources,
        17
    );
    assert_eq!(
        config.bikeshare.preprocessing.max_geometric_radius_metres,
        42
    );
}

#[test]
fn data_cli_validates_without_credentials_and_explicit_path_beats_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_via-data"))
        .args([
            "--config",
            concat!(env!("CARGO_MANIFEST_DIR"), "/config/via.toml"),
            "validate-config",
        ])
        .current_dir(std::env::temp_dir())
        .env("VIA_CONFIG", "/missing-env-config.toml")
        .env("VIA_ENV", "development")
        .env("TDX_CLIENT_ID", "")
        .env("TDX_CLIENT_SECRET", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn environment_port_override_is_validated() {
    let output = Command::new(env!("CARGO_BIN_EXE_via-data"))
        .args([
            "--config",
            concat!(env!("CARGO_MANIFEST_DIR"), "/config/via.toml"),
            "validate-config",
        ])
        .env("VIA_ENV", "development")
        .env("PORT", "0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("server.port"));
}

#[test]
fn compatibility_environment_values_override_toml_without_second_defaults() {
    let mut config = ViaConfig::from_toml(TOML).unwrap();
    config
        .apply_environment_values(
            Some("8088"),
            Some("https://tdx.example.com/api/"),
            Some("https://router.example.com/"),
        )
        .unwrap();
    assert_eq!(config.server.port, 8088);
    assert_eq!(config.tdx.api_base_url, "https://tdx.example.com/api/");
    assert_eq!(
        config
            .valhalla
            .provider_config(false)
            .unwrap()
            .base_url
            .as_str(),
        "https://router.example.com/"
    );
    config.apply_environment_values(None, None, None).unwrap();
    assert_eq!(config.server.port, 8088);
}

#[tokio::test]
async fn server_starts_with_supplied_config_and_environment_port_override() {
    use via_server::{
        data::{
            ViaStatic,
            via_static::{ViaStaticCounts, ViaStaticRevisions},
        },
        domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork},
        providers::tdx_bindings::TdxProviderBindings,
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = std::env::temp_dir().join(format!("via-startup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let network =
        TransitNetwork::from_artifact_json(include_str!("../fixtures/routing/network.json"))
            .unwrap()
            .with_revisions("static", "schedule");
    let artifact = ViaStatic {
        schema_version: 2,
        fetched_at: "2026-09-21T10:00:00Z".into(),
        sources: Default::default(),
        revisions: ViaStaticRevisions {
            transit_static: "static".into(),
            schedule: "schedule".into(),
            bike_share: "bike".into(),
        },
        counts: ViaStaticCounts {
            stops: network.stops.len(),
            routes: network.routes.len(),
            patterns: network.patterns.len(),
            trips: network.trips.len(),
            calendars: network.calendars.len(),
            frequencies: network.frequencies.len(),
            transfers: network.transfer_edges.len(),
            ..Default::default()
        },
        transit: network,
        provider_bindings: TdxProviderBindings::default(),
        bike_share: BikeShareNetwork::new(Vec::new()).with_revision("bike"),
    };
    artifact.validate().unwrap();
    let artifact_path = root.join("static.json");
    std::fs::write(&artifact_path, serde_json::to_vec(&artifact).unwrap()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}/", listener.local_addr().unwrap());
    let mock = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route(
                "/status",
                axum::routing::get(|| async { axum::Json(serde_json::json!({"has_tiles": true})) }),
            ),
        )
        .await
        .unwrap();
    });
    let config_path = root.join("via.toml");
    std::fs::write(
        &config_path,
        TOML.replace("http://127.0.0.1:8002/", &upstream),
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_via-server"))
            .current_dir(&root)
            .env("VIA_CONFIG", &config_path)
            .env("VIA_ENV", "development")
            .env("VIA_STATIC", &artifact_path)
            .env("VIA_VALHALLA_URL", &upstream)
            .env("VIA_TDX_BASE_URL", &upstream)
            .env("TDX_CLIENT_ID", "fixture")
            .env("TDX_CLIENT_SECRET", "fixture")
            .env("PORT", port.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    let mut response = None;
    for _ in 0..250 {
        if let Ok(result) = client
            .get(format!("http://127.0.0.1:{port}/openapi.json"))
            .send()
            .await
        {
            response = Some(result);
            break;
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("server exited: {status}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(response.unwrap().status().is_success());
    drop(child);
    mock.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn route_cli_reports_explicit_config_errors_before_loading_dependencies() {
    let path = std::env::temp_dir().join(format!("via-route-config-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&path, "invalid TOML").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_via-route"))
        .args([
            "--config",
            path.to_str().unwrap(),
            "--static",
            "/missing-artifact.json",
            "--from-lat",
            "25",
            "--from-lon",
            "121.5",
            "--to-lat",
            "25.1",
            "--to-lon",
            "121.6",
        ])
        .output()
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(path.to_str().unwrap()));
}

#[test]
fn obsolete_phase2_expansion_setting_is_rejected() {
    let body = TOML.replace("[routing]", "[routing]\nmax_phase2_expansions = 2000");
    let error = ViaConfig::from_toml(&body).unwrap_err().to_string();
    assert!(
        error.contains("unknown field `max_phase2_expansions`"),
        "{error}"
    );
}
