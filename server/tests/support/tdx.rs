use std::sync::Arc;

use axum::Router;
use via_server::tdx::{TdxClient, TdxCredentials};

pub async fn spawn_tdx_fixture(app: Router) -> Arc<TdxClient> {
    spawn_tdx_fixture_with_settings(app, via_server::config::ViaConfig::default().tdx).await
}

pub async fn spawn_tdx_fixture_with_settings(
    app: Router,
    settings: via_server::config::TdxSettings,
) -> Arc<TdxClient> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base_url = format!("http://{address}/");
    Arc::new(
        TdxClient::new(
            TdxCredentials::from_client_credentials("fixture-client", "fixture-secret"),
            via_server::tdx::TdxSettings {
                api_base_url: base_url.clone(),
                token_url: base_url.clone(),
                ..settings
            },
        )
        .unwrap(),
    )
}

#[allow(dead_code)]
pub fn failing_tdx_client() -> Arc<TdxClient> {
    Arc::new(
        TdxClient::new(
            TdxCredentials::from_client_credentials("fixture-client", "fixture-secret"),
            via_server::tdx::TdxSettings {
                api_base_url: "http://127.0.0.1:1/".into(),
                token_url: "http://127.0.0.1:1/".into(),
                ..via_server::config::ViaConfig::default().tdx
            },
        )
        .unwrap(),
    )
}
