use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use url::Url;
use via_server::{
    app::build_router_with_realtime_and_network_and_ttl,
    postgres::PostgresIdentityResolver,
    realtime::DEFAULT_REALTIME_CACHE_TTL,
    street::{GraphRevision, ValhallaConfig, ValhallaProvider},
    tdx::{TdxRealtimeConfig, TdxRealtimeProvider},
    transit::TransitNetwork,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();

    let environment = env::var("VIA_ENV")
        .map_err(|_| "VIA_ENV must be explicitly set to development, staging, or production")?;
    if !matches!(
        environment.as_str(),
        "development" | "staging" | "production"
    ) {
        return Err("VIA_ENV must be development, staging, or production".into());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "via_server=info".into()),
        )
        .without_time()
        .init();

    let graph_revision = env::var("VIA_STREET_GRAPH_REVISION")
        .map(GraphRevision::new)
        .map_err(|_| "VIA_STREET_GRAPH_REVISION must identify the immutable graph artifact")?;
    let tileset_last_modified = env::var("VIA_VALHALLA_TILESET_LAST_MODIFIED")
        .map_err(|_| "VIA_VALHALLA_TILESET_LAST_MODIFIED must pin the loaded graph artifact")?
        .parse()?;
    let mut config = ValhallaConfig::local(graph_revision, tileset_last_modified);
    if let Ok(base_url) = env::var("VIA_VALHALLA_URL") {
        config.base_url = Url::parse(&base_url)?;
    }
    if environment == "production" && config.base_url.scheme() != "https" {
        return Err("VIA_VALHALLA_URL must use HTTPS in production".into());
    }
    let provider = Arc::new(ValhallaProvider::new(config)?);
    let database_url = env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL must point to the pooled PostgreSQL endpoint")?;
    let max_database_connections = env::var("VIA_DATABASE_MAX_CONNECTIONS")
        .unwrap_or_else(|_| "4".into())
        .parse()?;
    let identities =
        Arc::new(PostgresIdentityResolver::connect(&database_url, max_database_connections).await?);
    let transit_network_path = env::var("VIA_TRANSIT_NETWORK")
        .map_err(|_| "VIA_TRANSIT_NETWORK must point to the immutable transit network JSON")?;
    let transit_network = TransitNetwork::from_artifact_json(
        &std::fs::read_to_string(&transit_network_path)
            .map_err(|_| "VIA_TRANSIT_NETWORK could not be read")?,
    )
    .map_err(|error| format!("VIA_TRANSIT_NETWORK is invalid: {error}"))?;
    transit_network
        .validate()
        .map_err(|error| format!("VIA_TRANSIT_NETWORK is invalid: {error}"))?;
    let tdx_bearer_token = env::var("VIA_TDX_BEARER_TOKEN")
        .map_err(|_| "VIA_TDX_BEARER_TOKEN must authenticate realtime requests")?;
    let tdx_base_url = Url::parse(
        &env::var("VIA_TDX_BASE_URL")
            .map_err(|_| "VIA_TDX_BASE_URL must point to the TDX API base URL")?,
    )?;
    if environment == "production" && tdx_base_url.scheme() != "https" {
        return Err("VIA_TDX_BASE_URL must use HTTPS in production".into());
    }
    let authority_namespace = env::var("VIA_TDX_AUTHORITY_NAMESPACE")
        .map_err(|_| "VIA_TDX_AUTHORITY_NAMESPACE must identify the TDX authority")?;
    let mut realtime = TdxRealtimeProvider::new(
        TdxRealtimeConfig {
            base_url: tdx_base_url,
            authority_namespace,
        },
        &tdx_bearer_token,
    )?
    .with_identity_resolver(identities);
    if let Ok(path) = env::var("VIA_TDX_SERVICE_CONDITION_PATH") {
        realtime = realtime.with_service_condition_path(path);
    }
    let cache_ttl = env::var("VIA_REALTIME_CACHE_TTL_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_REALTIME_CACHE_TTL);
    let router = build_router_with_realtime_and_network_and_ttl(
        provider,
        Arc::new(realtime),
        transit_network,
        cache_ttl,
    )
    .await?;
    let port = env::var("PORT")
        .unwrap_or_else(|_| "3000".into())
        .parse::<u16>()?;
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(port, "Via server listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
