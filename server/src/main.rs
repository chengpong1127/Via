use std::{env, net::SocketAddr, path::Path};
use via_server::{
    app::build_router,
    config::load_dotenv,
    data::load_static_artifact,
    runtime::{ProductionRuntimeOptions, build_production_runtime},
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

    let static_path = env::var("VIA_STATIC")
        .map_err(|_| "VIA_STATIC must point to a validated .via-static.json artifact")?;
    let via_static = load_static_artifact(Path::new(&static_path))
        .map_err(|error| format!("VIA_STATIC is invalid: {error}"))?;
    let (provider, runtime) =
        build_production_runtime(via_static, ProductionRuntimeOptions::default()).await?;
    let router = build_router(provider, runtime);
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
