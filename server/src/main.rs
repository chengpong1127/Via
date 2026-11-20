use std::net::SocketAddr;
use via_server::{
    app::{build_router, metrics, profiling, runtime::build_runtime, shutdown_signal},
    config::{RuntimeConfig, ServerConfig, ViaConfig, load_dotenv},
    data::load_static_artifact,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();

    let mut settings = ViaConfig::load(None)?;
    settings.apply_environment()?;
    let config = ServerConfig::from_settings(&settings)?;

    let _trace_guard = profiling::init(Some(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "via_server=info".into()),
    ))?;

    let metrics = metrics::install_prometheus()?;

    let via_static = load_static_artifact(&config.static_path)
        .map_err(|error| format!("VIA_STATIC is invalid: {error}"))?;
    let server_settings = settings.server.clone();
    let runtime_config = RuntimeConfig::from_settings(config.environment, settings)?;
    let runtime = build_runtime(via_static, runtime_config).await?;
    let router = build_router(runtime, metrics, config.environment, &server_settings);
    let address = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(port = config.port, "Via server listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
