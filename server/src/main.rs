use std::{env, net::SocketAddr, sync::Arc};

use url::Url;
use via_server::{
    app::build_router,
    street::{GraphRevision, ValhallaConfig, ValhallaProvider},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let provider = Arc::new(ValhallaProvider::new(config)?);
    let router = build_router(provider).await?;
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
