use std::sync::Arc;

use axum::Router;
use via_server::tdx::{TdxClient, TdxCredentials};

pub async fn spawn_tdx_fixture(app: Router) -> Arc<TdxClient> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base_url = format!("http://{address}/");
    Arc::new(
        TdxClient::with_urls(
            TdxCredentials::from_client_credentials("fixture-client", "fixture-secret"),
            &base_url,
            &base_url,
        )
        .unwrap(),
    )
}
