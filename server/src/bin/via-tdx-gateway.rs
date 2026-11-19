use std::{net::SocketAddr, sync::Arc};

use via_server::{
    app::shutdown_signal,
    config::{Environment, ViaConfig, load_dotenv, tdx_credentials},
    live::{CachedLiveData, DirectLiveData, gateway},
    metrics,
    tdx::TdxClient,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();

    let mut settings = ViaConfig::load(None)?;
    settings.apply_environment()?;
    settings.tdx.gateway.url = None;
    settings.validate_environment(Environment::required_from_env()?)?;

    let _trace_guard = via_server::profiling::init(Some(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "via_server=info,via_tdx_gateway=info".into()),
    ))?;

    let metrics = metrics::install_prometheus()?;

    let client = Arc::new(TdxClient::new(
        tdx_credentials(&settings.tdx)?,
        settings.tdx.clone(),
    )?);
    let live = Arc::new(CachedLiveData::new(
        Arc::new(DirectLiveData::new(client)),
        &settings.tdx,
    ));
    let router = gateway::router(live, metrics, settings.tdx.gateway.max_ids_per_request);

    let address = SocketAddr::from(([0, 0, 0, 0], settings.tdx.gateway.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(
        port = settings.tdx.gateway.port,
        "Via TDX gateway listening"
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
