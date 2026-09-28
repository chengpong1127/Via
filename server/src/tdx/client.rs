use std::{
    env, fmt,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use reqwest::{Client, Response, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

const DEFAULT_TDX_API_BASE_URL: &str = "https://tdx.transportdata.tw/api/basic/";
const DEFAULT_TDX_TOKEN_URL: &str =
    "https://tdx.transportdata.tw/auth/realms/TDXConnect/protocol/openid-connect/token";
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_RATE_LIMIT_RETRIES: usize = 2;

#[derive(Clone)]
pub struct TdxCredentials {
    client_id: String,
    client_secret: String,
}

impl TdxCredentials {
    pub fn from_client_credentials(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
        }
    }

    pub fn from_env() -> Result<Self, TdxCredentialsError> {
        let client_id = required_env("TDX_CLIENT_ID")?;
        let client_secret = required_env("TDX_CLIENT_SECRET")?;
        Ok(Self {
            client_id,
            client_secret,
        })
    }
}

impl fmt::Debug for TdxCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TdxCredentials")
            .field("client_id", &"<redacted>")
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

fn required_env(name: &'static str) -> Result<String, TdxCredentialsError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(TdxCredentialsError::Missing(name)),
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TdxCredentialsError {
    #[error("{0} is not configured. Set it in .env or the process environment.")]
    Missing(&'static str),
}

#[derive(Debug, Error)]
pub enum TdxClientError {
    #[error(transparent)]
    Credentials(#[from] TdxCredentialsError),
    #[error("TDX OAuth configuration is invalid")]
    InvalidConfiguration,
    #[error("TDX OAuth request failed")]
    OAuthRequest(#[source] reqwest::Error),
    #[error("TDX OAuth response was invalid")]
    InvalidOAuthResponse(#[source] reqwest::Error),
    #[error("TDX API request failed")]
    ApiRequest(#[source] reqwest::Error),
    #[error("TDX API rate limit reached")]
    RateLimited,
    #[error("TDX API rate limit reached")]
    RateLimitedWithRetryAfter(Duration),
    #[error("TDX API response exceeded the development limit")]
    ResponseTooLarge,
    #[error("TDX API response was not valid JSON")]
    InvalidJson(#[source] serde_json::Error),
    #[error("TDX download could not write its destination")]
    DownloadWrite(#[source] std::io::Error),
}

#[derive(Clone)]
pub struct TdxClient {
    client: Client,
    download_client: Client,
    credentials: TdxCredentials,
    api_base_url: Url,
    token_url: Url,
    token: Arc<Mutex<Option<CachedToken>>>,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

impl TdxClientError {
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimitedWithRetryAfter(retry_after) => Some(*retry_after),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default = "default_token_lifetime")]
    expires_in: u64,
}

fn default_token_lifetime() -> u64 {
    300
}

impl TdxClient {
    pub fn new(credentials: TdxCredentials) -> Result<Self, TdxClientError> {
        Self::with_urls(credentials, DEFAULT_TDX_API_BASE_URL, DEFAULT_TDX_TOKEN_URL)
    }

    pub fn with_api_base_url(
        credentials: TdxCredentials,
        api_base_url: &str,
    ) -> Result<Self, TdxClientError> {
        Self::with_urls(credentials, api_base_url, DEFAULT_TDX_TOKEN_URL)
    }

    pub fn with_urls(
        credentials: TdxCredentials,
        api_base_url: &str,
        token_url: &str,
    ) -> Result<Self, TdxClientError> {
        let api_base_url =
            Url::parse(api_base_url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        let token_url = Url::parse(token_url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        if api_base_url.host_str().is_none() || token_url.host_str().is_none() {
            return Err(TdxClientError::InvalidConfiguration);
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(20))
            .user_agent("via-data/1.0")
            .build()
            .map_err(TdxClientError::ApiRequest)?;
        let download_client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(DOWNLOAD_TIMEOUT)
            .user_agent("via-data/1.0")
            .build()
            .map_err(TdxClientError::ApiRequest)?;
        Ok(Self {
            client,
            download_client,
            credentials,
            api_base_url,
            token_url,
            token: Arc::new(Mutex::new(None)),
        })
    }

    async fn access_token(&self) -> Result<String, TdxClientError> {
        let mut cached = self.token.lock().await;
        if cached
            .as_ref()
            .is_some_and(|token| token.expires_at > Instant::now())
        {
            return Ok(cached.as_ref().expect("checked cached token").value.clone());
        }

        let response = self
            .client
            .post(self.token_url.clone())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.credentials.client_id.as_str()),
                ("client_secret", self.credentials.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(TdxClientError::OAuthRequest)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(rate_limited(response.headers()));
        }
        let response = response
            .error_for_status()
            .map_err(TdxClientError::OAuthRequest)?;
        let token = response
            .json::<OAuthTokenResponse>()
            .await
            .map_err(TdxClientError::InvalidOAuthResponse)?;
        if token.access_token.trim().is_empty() {
            return Err(TdxClientError::InvalidConfiguration);
        }
        let lifetime = Duration::from_secs(token.expires_in.max(60));
        let refresh_margin = Duration::from_secs(30).min(lifetime / 2);
        let value = token.access_token;
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + lifetime.saturating_sub(refresh_margin),
        });
        Ok(value)
    }

    fn api_url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, TdxClientError> {
        let mut url = self
            .api_base_url
            .join(path)
            .map_err(|_| TdxClientError::InvalidConfiguration)?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        Ok(url)
    }

    async fn authenticated_get(&self, url: Url) -> Result<Response, TdxClientError> {
        self.authenticated_get_with(&self.client, url).await
    }

    async fn authenticated_get_with(
        &self,
        client: &Client,
        url: Url,
    ) -> Result<Response, TdxClientError> {
        let token = self.access_token().await?;
        let mut retries = 0;
        loop {
            let response = client
                .get(url.clone())
                .bearer_auth(&token)
                .send()
                .await
                .map_err(TdxClientError::ApiRequest)?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                if let Some(delay) = retry_after(response.headers())
                    && retries < MAX_RATE_LIMIT_RETRIES
                {
                    retries += 1;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(rate_limited(response.headers()));
            }
            return response
                .error_for_status()
                .map_err(TdxClientError::ApiRequest);
        }
    }

    pub async fn get_bytes(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<bytes::Bytes, TdxClientError> {
        let response = self.authenticated_get(self.api_url(path, query)?).await?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(TdxClientError::ResponseTooLarge);
        }
        let body = response.bytes().await.map_err(TdxClientError::ApiRequest)?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(TdxClientError::ResponseTooLarge);
        }
        Ok(body)
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, TdxClientError> {
        let body = self.get_bytes(path, query).await?;
        serde_json::from_slice(&body).map_err(TdxClientError::InvalidJson)
    }

    pub async fn download_url_to_path(
        &self,
        url: &str,
        destination: &Path,
    ) -> Result<u64, TdxClientError> {
        let url = Url::parse(url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        let mut response = self
            .authenticated_get_with(&self.download_client, url)
            .await?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await
            .map_err(TdxClientError::DownloadWrite)?;
        let mut bytes_written = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(TdxClientError::ApiRequest)? {
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(TdxClientError::DownloadWrite)?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(TdxClientError::DownloadWrite)?;
        Ok(bytes_written)
    }
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(
        retry_at
            .duration_since(SystemTime::now())
            .unwrap_or_default(),
    )
}

fn rate_limited(headers: &reqwest::header::HeaderMap) -> TdxClientError {
    retry_after(headers)
        .map(TdxClientError::RateLimitedWithRetryAfter)
        .unwrap_or(TdxClientError::RateLimited)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{Request, Response, StatusCode},
        routing::any,
    };

    use super::{MAX_RESPONSE_BYTES, TdxClient, TdxCredentials};

    async fn mock_response(
        State(body): State<Arc<Vec<u8>>>,
        request: Request<Body>,
    ) -> Response<Body> {
        if request.uri().path() == "/token" {
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"access_token":"mock-token","expires_in":300}"#,
                ))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body.as_ref().clone()))
            .unwrap()
    }

    async fn rate_limited_once(
        State((body, requests)): State<(Arc<Vec<u8>>, Arc<AtomicUsize>)>,
        request: Request<Body>,
    ) -> Response<Body> {
        if request.uri().path() == "/token" {
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"access_token":"mock-token","expires_in":300}"#,
                ))
                .unwrap();
        }
        if requests.fetch_add(1, Ordering::SeqCst) == 0 {
            return Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header("retry-after", "0")
                .body(Body::empty())
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body.as_ref().clone()))
            .unwrap()
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = TdxCredentials {
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("client-id-value"));
        assert!(!debug.contains("client-secret-value"));
        assert!(debug.contains("<redacted>"));
    }

    #[tokio::test]
    async fn retries_rate_limited_get_after_retry_after() {
        let expected = br#"{"available":true}"#.to_vec();
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(rate_limited_once))
            .with_state((Arc::new(expected.clone()), requests.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{address}");
        let client = TdxClient::with_urls(
            TdxCredentials::from_client_credentials("client", "secret"),
            &format!("{base_url}/api/"),
            &format!("{base_url}/token"),
        )
        .unwrap();

        let response = client.get_bytes("availability", &[]).await.unwrap();

        assert_eq!(response.as_ref(), expected);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn streamed_download_exceeds_json_response_limit() {
        let expected_size = MAX_RESPONSE_BYTES + 1;
        let body = Arc::new(vec![0x5a; expected_size]);
        let app = Router::new().fallback(any(mock_response)).with_state(body);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{address}");
        let client = TdxClient::with_urls(
            TdxCredentials::from_client_credentials("client", "secret"),
            &format!("{base_url}/api/"),
            &format!("{base_url}/token"),
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "via-tdx-large-download-{}.bin",
            uuid::Uuid::new_v4().simple()
        ));

        let bytes_written = client
            .download_url_to_path(&format!("{base_url}/feed"), &path)
            .await
            .unwrap();
        assert_eq!(bytes_written, expected_size as u64);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), bytes_written);
        std::fs::remove_file(path).unwrap();
    }
}
