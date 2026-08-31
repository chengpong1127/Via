use std::{env, net::SocketAddr, sync::Arc};

use url::Url;
use via_server::{
    app::build_router_with_services,
    postgres::PostgresSnapshotStore,
    refresh::RefreshCoordinator,
    snapshot::NetworkSnapshot,
    street::{GraphRevision, ValhallaConfig, ValhallaProvider},
    tdx::{TdxFeedConfig, TdxFeedSource},
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
    let database_url = env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL must point to the pooled PostgreSQL endpoint")?;
    let max_database_connections = env::var("VIA_DATABASE_MAX_CONNECTIONS")
        .unwrap_or_else(|_| "4".into())
        .parse()?;
    let snapshot_store =
        Arc::new(PostgresSnapshotStore::connect(&database_url, max_database_connections).await?);
    let now = time::OffsetDateTime::now_utc();
    snapshot_store
        .ensure_initial(NetworkSnapshot {
            id: "initial-snapshot".into(),
            created_at: now,
            live_cutoff: now,
            static_network_revision: env::var("VIA_STATIC_NETWORK_REVISION")
                .map_err(|_| "VIA_STATIC_NETWORK_REVISION must identify the immutable network")?,
            schedule_revision: env::var("VIA_SCHEDULE_REVISION")
                .map_err(|_| "VIA_SCHEDULE_REVISION must identify the immutable schedule")?,
            freshness_policy_version: "1".into(),
            publication_version: 1,
            feed_evidence: Vec::new(),
        })
        .await?;
    let refresh_token = env::var("VIA_REFRESH_TOKEN")
        .map_err(|_| "VIA_REFRESH_TOKEN must authenticate scheduler refreshes")?;
    let feed_configs: Vec<TdxFeedConfig> = serde_json::from_str(
        &env::var("VIA_TDX_FEEDS")
            .map_err(|_| "VIA_TDX_FEEDS must be a JSON array of capability and URL objects")?,
    )?;
    if feed_configs.is_empty() {
        return Err("VIA_TDX_FEEDS must configure at least one authoritative feed".into());
    }
    let tdx_bearer_token = env::var("VIA_TDX_BEARER_TOKEN")
        .map_err(|_| "VIA_TDX_BEARER_TOKEN must authenticate authoritative feed requests")?;
    let feed_sources = feed_configs
        .into_iter()
        .map(|config| {
            TdxFeedSource::new(config, &tdx_bearer_token)
                .map(|source| Arc::new(source) as Arc<dyn via_server::refresh::FeedSource>)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let refresh = RefreshCoordinator::new(snapshot_store.clone(), feed_sources, 4);
    let router =
        build_router_with_services(provider, snapshot_store, Some(refresh), Some(refresh_token))
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
