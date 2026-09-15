//! Process configuration shared by development tooling and providers.

use std::{
    env, fmt,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use reqwest::{Client, StatusCode, header::RETRY_AFTER};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

const DEFAULT_TDX_API_BASE_URL: &str = "https://tdx.transportdata.tw/api/basic/";
const DEFAULT_TDX_TOKEN_URL: &str =
    "https://tdx.transportdata.tw/auth/realms/TDXConnect/protocol/openid-connect/token";
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Credentials used to obtain a process-local TDX OAuth access token.
///
/// The fields intentionally have no public accessors that expose them through
/// formatting. Providers receive the token from [`TdxClient`] instead.
#[derive(Clone)]
pub struct TdxCredentials {
    client_id: String,
    client_secret: String,
}

impl TdxCredentials {
    pub fn from_env() -> Result<Self, TdxCredentialsError> {
        let client_id = required_env("TDX_CLIENT_ID")?;
        let client_secret = required_env("TDX_CLIENT_SECRET")?;
        Ok(Self {
            client_id,
            client_secret,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            client_id: "test-client-id".into(),
            client_secret: "test-client-secret".into(),
        }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub(crate) fn client_secret(&self) -> &str {
        &self.client_secret
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

/// Load the repository-local dotenv file when present. Existing process
/// environment values keep dotenvy's normal precedence.
pub fn load_dotenv() {
    dotenvy::dotenv().ok();
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../.env")).ok();
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
}

#[derive(Clone)]
pub struct TdxClient {
    client: Client,
    credentials: TdxCredentials,
    api_base_url: Url,
    token_url: Url,
    token: Arc<Mutex<Option<CachedToken>>>,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct TdxRawResponse {
    pub url: Url,
    pub body: Vec<u8>,
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
        Ok(Self {
            client,
            credentials,
            api_base_url,
            token_url,
            token: Arc::new(Mutex::new(None)),
        })
    }

    /// Return a cached process-local OAuth token, refreshing it before expiry.
    pub async fn access_token(&self) -> Result<String, TdxClientError> {
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
                ("client_id", self.credentials.client_id()),
                ("client_secret", self.credentials.client_secret()),
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

    pub async fn get_raw(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<TdxRawResponse, TdxClientError> {
        let mut url = self
            .api_base_url
            .join(path)
            .map_err(|_| TdxClientError::InvalidConfiguration)?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        let token = self.access_token().await?;
        let response = self
            .client
            .get(url.clone())
            .bearer_auth(token)
            .send()
            .await
            .map_err(TdxClientError::ApiRequest)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(rate_limited(response.headers()));
        }
        let response = response
            .error_for_status()
            .map_err(TdxClientError::ApiRequest)?;
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
        Ok(TdxRawResponse {
            url,
            body: body.to_vec(),
        })
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
    use super::TdxCredentials;

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
}
