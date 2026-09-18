use std::{env, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use url::Url;
use via_server::{
    app::build_router_with_realtime_and_network_and_ttl_and_tdx,
    config::{TdxClient, TdxCredentials, load_dotenv},
    data::load_network_artifact,
    health::DependencyHealth,
    providers::realtime::DEFAULT_REALTIME_CACHE_TTL,
    providers::{TdxRealtimeProvider, ValhallaConfig, ValhallaProvider},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();

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

    let valhalla_graph_revision = env::var("VIA_VALHALLA_GRAPH_REVISION")
        .map_err(|_| "VIA_VALHALLA_GRAPH_REVISION must identify the immutable graph artifact")?;
    let tileset_last_modified = env::var("VIA_VALHALLA_TILESET_LAST_MODIFIED")
        .map_err(|_| "VIA_VALHALLA_TILESET_LAST_MODIFIED must pin the loaded graph artifact")?
        .parse()?;
    let mut config = ValhallaConfig::local(valhalla_graph_revision, tileset_last_modified);
    if let Ok(base_url) = env::var("VIA_VALHALLA_URL") {
        config.base_url = Url::parse(&base_url)?;
    }
    if environment == "production" && config.base_url.scheme() != "https" {
        return Err("VIA_VALHALLA_URL must use HTTPS in production".into());
    }
    let provider = Arc::new(ValhallaProvider::new(config)?);
    if !provider.healthy().await {
        return Err("Valhalla is not healthy for the configured graph artifact".into());
    }
    let transit_network_path = env::var("VIA_TRANSIT_NETWORK")
        .map_err(|_| "VIA_TRANSIT_NETWORK must point to the immutable transit network JSON")?;
    let transit_network = load_network_artifact(Path::new(&transit_network_path))
        .map_err(|error| format!("VIA_TRANSIT_NETWORK is invalid: {error}"))?;
    let tdx_base_url_value = env::var("VIA_TDX_BASE_URL")
        .map_err(|_| "VIA_TDX_BASE_URL must point to the TDX API base URL")?;
    let tdx_base_url = Url::parse(&tdx_base_url_value)?;
    if environment == "production" && tdx_base_url.scheme() != "https" {
        return Err("VIA_TDX_BASE_URL must use HTTPS in production".into());
    }
    let tdx_client = Arc::new(TdxClient::with_api_base_url(
        TdxCredentials::from_env()?,
        &tdx_base_url_value,
    )?);
    let mut realtime = TdxRealtimeProvider::new(tdx_client.clone());
    if let Ok(path) = env::var("VIA_TDX_SERVICE_CONDITION_PATH") {
        realtime = realtime.with_service_condition_path(path);
    }
    let cache_ttl = env::var("VIA_REALTIME_CACHE_TTL_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_REALTIME_CACHE_TTL);
    let router = build_router_with_realtime_and_network_and_ttl_and_tdx(
        provider,
        Arc::new(realtime),
        Some(tdx_client),
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
