use std::net::SocketAddr;
use via_server::{
    app::build_router,
    config::{RuntimeConfig, ServerConfig, ViaConfig, load_dotenv},
    data::load_static_artifact,
    metrics,
    runtime::{RuntimeOptions, build_runtime},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();

    let mut settings = ViaConfig::load(None)?;
    settings.apply_environment()?;
    let config = ServerConfig::from_settings(&settings)?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "via_server=info".into()),
        )
        .without_time()
        .init();

    let metrics = metrics::install_prometheus()?;

    let via_static = load_static_artifact(&config.static_path)
        .map_err(|error| format!("VIA_STATIC is invalid: {error}"))?;
    let runtime_config = RuntimeConfig::from_settings(config.environment, settings.clone())?;
    let runtime = build_runtime(via_static, RuntimeOptions::default(), runtime_config).await?;
    let router = build_router(
        runtime.routing,
        runtime.health,
        metrics,
        config.environment,
        &settings.server,
    );
    let address = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(port = config.port, "Via server listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.expect("wait for Ctrl-C signal");
            }
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("wait for Ctrl-C signal");
}
