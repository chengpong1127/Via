use std::net::SocketAddr;

use via_server::{
    app::{metrics, profiling, runtime::direct_live_data, shutdown_signal},
    config::{Environment, ViaConfig, load_dotenv, tdx_credentials},
    live::gateway,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();

    let settings = ViaConfig::load_for_with(None, Environment::required_from_env()?, |settings| {
        settings.tdx.gateway.url = None;
    })?;

    let _trace_guard = profiling::init(Some(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "via_server=info,via_tdx_gateway=info".into()),
    ))?;

    let metrics = metrics::install_prometheus()?;

    let live = direct_live_data(tdx_credentials()?, &settings.tdx)?;
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
