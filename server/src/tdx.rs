use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use reqwest::{
    Client, Response, StatusCode,
    header::{RANGE, RETRY_AFTER},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

pub const BIKE_CITIES: &[(&str, &str)] = &[
    ("ChanghuaCounty", "changhua-county"),
    ("Chiayi", "chiayi"),
    ("ChiayiCounty", "chiayi-county"),
    ("Hsinchu", "hsinchu"),
    ("HsinchuCounty", "hsinchu-county"),
    ("Kaohsiung", "kaohsiung"),
    ("MiaoliCounty", "miaoli-county"),
    ("NewTaipei", "new-taipei"),
    ("PingtungCounty", "pingtung-county"),
    ("Taichung", "taichung"),
    ("Tainan", "tainan"),
    ("Taipei", "taipei"),
    ("TaitungCounty", "taitung-county"),
    ("Taoyuan", "taoyuan"),
    ("YunlinCounty", "yunlin-county"),
];

pub fn is_bike_city(city: &str) -> bool {
    BIKE_CITIES.iter().any(|(scope, _)| *scope == city)
}

pub fn tdx_bike_station_parts(id: &crate::domain::ids::BikeStationId) -> Option<(&str, &str)> {
    let (city, uid) = id.as_str().strip_prefix("tdx:")?.split_once(':')?;
    if !is_bike_city(city)
        || uid.is_empty()
        || uid.trim() != uid
        || uid.contains(':')
        || uid.chars().any(char::is_control)
    {
        return None;
    }
    Some((city, uid))
}

pub(crate) fn odata_in_filter(field: &str, values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!("{field} in ({values})")
}

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
    fetch_client: Client,
    download_client: Client,
    credentials: TdxCredentials,
    api_base_url: Url,
    token_url: Url,
    token: Arc<Mutex<Option<CachedToken>>>,
    settings: TdxSettings,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
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
    pub fn new(credentials: TdxCredentials, settings: TdxSettings) -> Result<Self, TdxClientError> {
        let api_base_url =
            Url::parse(&settings.api_base_url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        let token_url =
            Url::parse(&settings.token_url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        if api_base_url.host_str().is_none() || token_url.host_str().is_none() {
            return Err(TdxClientError::InvalidConfiguration);
        }
        Ok(Self {
            client: http_client(
                Duration::from_millis(settings.connect_timeout_ms),
                Duration::from_millis(settings.request_timeout_ms),
                Policy::none(),
            )?,
            // No redirects on purpose: a redirect target would be requested with the same query
            // string and could carry it to another host, so a 3xx surfaces as an error instead.
            fetch_client: http_client(
                Duration::from_millis(settings.fetch.connect_timeout_ms),
                Duration::from_millis(settings.fetch.request_timeout_ms),
                Policy::none(),
            )?,
            download_client: http_client(
                Duration::from_millis(settings.fetch.connect_timeout_ms),
                Duration::from_secs(settings.download_timeout_seconds),
                Policy::default(),
            )?,
            credentials,
            api_base_url,
            token_url,
            token: Arc::new(Mutex::new(None)),
            settings,
        })
    }

    pub fn settings(&self) -> &TdxSettings {
        &self.settings
    }

    async fn access_token_with(&self, client: &Client) -> Result<String, TdxClientError> {
        let mut cached = self.token.lock().await;
        if cached
            .as_ref()
            .is_some_and(|token| token.expires_at > Instant::now())
        {
            return Ok(cached.as_ref().expect("checked cached token").value.clone());
        }

        let response = client
            .post(self.token_url.clone())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.credentials.client_id.as_str()),
                ("client_secret", self.credentials.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|error| TdxClientError::OAuthRequest(redact_url(error)))?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(rate_limited(response.headers()));
        }
        let response = response
            .error_for_status()
            .map_err(|error| TdxClientError::OAuthRequest(redact_url(error)))?;
        let token = response
            .json::<OAuthTokenResponse>()
            .await
            .map_err(|error| TdxClientError::InvalidOAuthResponse(redact_url(error)))?;
        if token.access_token.trim().is_empty() {
            return Err(TdxClientError::InvalidConfiguration);
        }
        let lifetime = Duration::from_secs(
            token
                .expires_in
                .max(self.settings.minimum_token_lifetime_seconds),
        );
        let refresh_margin =
            Duration::from_secs(self.settings.token_refresh_margin_seconds).min(lifetime / 2);
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

    async fn authenticated_get_with(
        &self,
        client: &Client,
        url: Url,
        max_rate_limit_retries: usize,
        range_from: Option<(u64, &str)>,
    ) -> Result<Response, TdxClientError> {
        let mut token = self.access_token_with(client).await?;
        let mut retries = 0;
        let mut token_refreshed = false;
        loop {
            let mut request = client.get(url.clone()).bearer_auth(&token);
            if let Some((offset, validator)) = range_from {
                request = request
                    .header(RANGE, format!("bytes={offset}-"))
                    .header(reqwest::header::IF_RANGE, validator);
            }
            let response = request
                .send()
                .await
                .map_err(|error| TdxClientError::ApiRequest(redact_url(error)))?;
            if response.status() == StatusCode::UNAUTHORIZED && !token_refreshed {
                // The key may have been rotated or the token revoked: drop it and retry once.
                token_refreshed = true;
                {
                    let mut cached = self.token.lock().await;
                    // A concurrent request may already have replaced the rejected token.
                    if cached.as_ref().is_some_and(|c| c.value == token) {
                        *cached = None;
                    }
                }
                token = self.access_token_with(client).await?;
                continue;
            }
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                if let Some(delay) = retry_after(response.headers())
                    && retries < max_rate_limit_retries
                {
                    retries += 1;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(rate_limited(response.headers()));
            }
            return response
                .error_for_status()
                .map_err(|error| TdxClientError::ApiRequest(redact_url(error)));
        }
    }

    pub async fn get_bytes(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<bytes::Bytes, TdxClientError> {
        self.get_url_bytes(
            self.api_url(path, query)?,
            self.settings.max_rate_limit_retries,
        )
        .await
    }

    async fn get_url_bytes(
        &self,
        url: Url,
        max_rate_limit_retries: usize,
    ) -> Result<bytes::Bytes, TdxClientError> {
        self.read_limited(&self.client, url, max_rate_limit_retries)
            .await
    }

    async fn read_limited(
        &self,
        client: &Client,
        url: Url,
        max_rate_limit_retries: usize,
    ) -> Result<bytes::Bytes, TdxClientError> {
        let response = self
            .authenticated_get_with(client, url, max_rate_limit_retries, None)
            .await?;
        if response
            .content_length()
            .is_some_and(|length| length > self.settings.max_response_bytes as u64)
        {
            return Err(TdxClientError::ResponseTooLarge);
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| TdxClientError::ApiRequest(redact_url(error)))?;
        if body.len() > self.settings.max_response_bytes {
            return Err(TdxClientError::ResponseTooLarge);
        }
        Ok(body)
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, TdxClientError> {
        let body = self.get_url_bytes(self.api_url(path, query)?, 0).await?;
        serde_json::from_slice(&body).map_err(TdxClientError::InvalidJson)
    }

    pub async fn fetch_bytes(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<bytes::Bytes, TdxClientError> {
        self.read_limited(&self.fetch_client, self.api_url(path, query)?, 0)
            .await
    }

    pub async fn download_to_path(
        &self,
        url: &str,
        destination: &Path,
        resume: bool,
    ) -> Result<u64, TdxClientError> {
        let url = Url::parse(url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        let mut validator_path = destination.as_os_str().to_owned();
        validator_path.push(".validator");
        let validator_path = PathBuf::from(validator_path);
        let stored_validator = if resume {
            tokio::fs::read_to_string(&validator_path)
                .await
                .ok()
                .filter(|value| !value.is_empty())
        } else {
            None
        };
        let existing = match stored_validator {
            Some(_) => tokio::fs::metadata(destination)
                .await
                .map(|metadata| metadata.len())
                .unwrap_or(0),
            None => 0,
        };
        let attempt = existing
            .gt(&0)
            .then(|| stored_validator.as_deref().map(|value| (existing, value)))
            .flatten();
        let mut response = match self
            .authenticated_get_with(&self.download_client, url.clone(), 0, attempt)
            .await
        {
            Err(TdxClientError::ApiRequest(error))
                if attempt.is_some()
                    && error.status() == Some(StatusCode::RANGE_NOT_SATISFIABLE) =>
            {
                self.authenticated_get_with(&self.download_client, url.clone(), 0, None)
                    .await?
            }
            other => other?,
        };
        let mut resuming = attempt.is_some()
            && response.status() == StatusCode::PARTIAL_CONTENT
            && content_range_start(response.headers()) == Some(existing);
        if attempt.is_some() && !resuming && response.status() == StatusCode::PARTIAL_CONTENT {
            response = self
                .authenticated_get_with(&self.download_client, url, 0, None)
                .await?;
            resuming = false;
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true);
        if resuming {
            options.append(true);
        } else {
            options.truncate(true);
            match response_validator(response.headers()) {
                Some(value) => tokio::fs::write(&validator_path, value)
                    .await
                    .map_err(TdxClientError::DownloadWrite)?,
                None => {
                    let _ = tokio::fs::remove_file(&validator_path).await;
                }
            }
        }
        let mut file = options
            .open(destination)
            .await
            .map_err(TdxClientError::DownloadWrite)?;
        let mut bytes_written = if resuming { existing } else { 0 };
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| TdxClientError::ApiRequest(redact_url(error)))?
        {
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(TdxClientError::DownloadWrite)?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(TdxClientError::DownloadWrite)?;
        let _ = tokio::fs::remove_file(&validator_path).await;
        Ok(bytes_written)
    }
}

fn redact_url(mut error: reqwest::Error) -> reqwest::Error {
    if let Some(url) = error.url_mut() {
        url.set_query(None);
        url.set_fragment(None);
    }
    error
}

fn content_range_start(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn response_validator(headers: &reqwest::header::HeaderMap) -> Option<String> {
    [reqwest::header::ETAG, reqwest::header::LAST_MODIFIED]
        .iter()
        .find_map(|name| headers.get(name)?.to_str().ok().map(str::to_owned))
}

impl TdxClientError {
    pub fn is_transient(&self) -> bool {
        match self {
            Self::OAuthRequest(error) | Self::ApiRequest(error) => {
                error.status().is_none_or(|status| {
                    status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT
                })
            }
            Self::RateLimited | Self::RateLimitedWithRetryAfter(_) => true,
            _ => false,
        }
    }
}

fn http_client(
    connect_timeout: Duration,
    timeout: Duration,
    redirects: Policy,
) -> Result<Client, TdxClientError> {
    Client::builder()
        .redirect(redirects)
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        .user_agent("via-data/1.0")
        .build()
        .map_err(|error| TdxClientError::ApiRequest(redact_url(error)))
}

/// Longest pause a `Retry-After` header may impose on this process.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    parse_retry_after(headers).map(|delay| delay.min(MAX_RETRY_AFTER))
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TdxSettings {
    pub api_base_url: String,
    pub token_url: String,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub download_timeout_seconds: u64,
    pub max_response_bytes: usize,
    pub max_rate_limit_retries: usize,
    pub minimum_token_lifetime_seconds: u64,
    pub token_refresh_margin_seconds: u64,
    pub bike_catalog_page_size: usize,
    pub rate_limit_backoff_seconds: u64,
    pub fetch: TdxFetchSettings,
    pub batch: TdxBatchSettings,
    pub realtime: RealtimeSettings,
    pub bikeshare: BikeAvailabilitySettings,
    pub gateway: TdxGatewaySettings,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TdxFetchSettings {
    pub requests_per_minute: u32,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub max_attempts: u32,
    pub retry_backoff_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TdxGatewaySettings {
    pub url: Option<String>,
    pub port: u16,
    pub max_ids_per_request: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TdxBatchSettings {
    pub bike_stations: usize,
    pub bus_stops: usize,
    pub metro_pairs: usize,
    pub metro_response_limit: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeSettings {
    pub fresh_seconds: u32,
    pub degraded_seconds: u32,
    pub max_clock_skew_seconds: u32,
    pub cache_ttl_seconds: u64,
    pub cache_max_bytes: u64,
}

impl RealtimeSettings {
    pub fn freshness_policy(&self) -> crate::domain::realtime::FreshnessPolicy {
        crate::domain::realtime::FreshnessPolicy {
            fresh_through: time::Duration::seconds(i64::from(self.fresh_seconds)),
            degraded_through: time::Duration::seconds(i64::from(self.degraded_seconds)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BikeAvailabilitySettings {
    pub cache_ttl_seconds: u64,
    pub cache_capacity: u64,
    pub max_concurrent_cities: usize,
    pub max_age_seconds: u32,
    pub max_clock_skew_seconds: u32,
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

    use super::{TdxClient, TdxCredentials};

    fn token_response() -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"access_token":"mock-token","expires_in":300}"#,
            ))
            .unwrap()
    }

    async fn spawn_client(app: Router) -> (TdxClient, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{address}");
        let client = TdxClient::new(
            TdxCredentials::from_client_credentials("client", "secret"),
            crate::tdx::TdxSettings {
                api_base_url: format!("{base_url}/api/"),
                token_url: format!("{base_url}/token"),
                ..crate::config::ViaConfig::default().tdx
            },
        )
        .unwrap();
        (client, base_url)
    }

    async fn mock_response(
        State(body): State<Arc<Vec<u8>>>,
        request: Request<Body>,
    ) -> Response<Body> {
        if request.uri().path() == "/token" {
            return token_response();
        }
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body.as_ref().clone()))
            .unwrap()
    }

    async fn versioned_response(
        State(versions): State<Arc<(Vec<u8>, &'static str)>>,
        request: Request<Body>,
    ) -> Response<Body> {
        if request.uri().path() == "/token" {
            return token_response();
        }
        let (body, etag) = (&versions.0, versions.1);
        let headers = request.headers();
        let offset = headers
            .get("range")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("bytes="))
            .and_then(|value| value.strip_suffix('-'))
            .and_then(|value| value.parse::<usize>().ok());
        let validator_matches = headers
            .get("if-range")
            .is_none_or(|value| value.to_str().ok() == Some(etag));
        match offset {
            Some(offset) if validator_matches => Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header("etag", etag)
                .header(
                    "content-range",
                    format!("bytes {offset}-{}/{}", body.len() - 1, body.len()),
                )
                .body(Body::from(body[offset..].to_vec()))
                .unwrap(),
            _ => Response::builder()
                .status(StatusCode::OK)
                .header("etag", etag)
                .body(Body::from(body.clone()))
                .unwrap(),
        }
    }

    fn partial_download(name: &str, content: &[u8], validator: Option<&str>) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "via-tdx-{name}-{}.bin",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, content).unwrap();
        if let Some(validator) = validator {
            let mut sidecar = path.clone().into_os_string();
            sidecar.push(".validator");
            std::fs::write(sidecar, validator).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn resume_never_splices_a_regenerated_file() {
        let new = vec![2u8; 4096];
        let app = Router::new()
            .fallback(any(versioned_response))
            .with_state(Arc::new((new.clone(), "\"v2\"")));
        let (client, base_url) = spawn_client(app).await;
        let path = partial_download("splice", &[1u8; 1000], Some("\"v1\""));

        client
            .download_to_path(&format!("{base_url}/feed"), &path, true)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), new);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn resume_without_a_stored_validator_restarts() {
        let new = vec![2u8; 4096];
        let app = Router::new()
            .fallback(any(versioned_response))
            .with_state(Arc::new((new.clone(), "\"v2\"")));
        let (client, base_url) = spawn_client(app).await;
        let path = partial_download("novalidator", &[1u8; 1000], None);

        client
            .download_to_path(&format!("{base_url}/feed"), &path, true)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), new);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn resumed_download_appends_the_missing_tail() {
        let body = (0..=255u8).cycle().take(4096).collect::<Vec<u8>>();
        let app = Router::new()
            .fallback(any(versioned_response))
            .with_state(Arc::new((body.clone(), "\"v1\"")));
        let (client, base_url) = spawn_client(app).await;
        let path = std::env::temp_dir().join(format!(
            "via-tdx-resume-{}.bin",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, &body[..1000]).unwrap();
        let mut validator = path.clone().into_os_string();
        validator.push(".validator");
        std::fs::write(&validator, "\"v1\"").unwrap();

        let bytes_written = client
            .download_to_path(&format!("{base_url}/feed"), &path, true)
            .await
            .unwrap();

        assert_eq!(bytes_written, body.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), body);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn download_without_resume_restarts_from_the_beginning() {
        let body = (0..=255u8).cycle().take(4096).collect::<Vec<u8>>();
        let app = Router::new()
            .fallback(any(versioned_response))
            .with_state(Arc::new((body.clone(), "\"v1\"")));
        let (client, base_url) = spawn_client(app).await;
        let path = std::env::temp_dir().join(format!(
            "via-tdx-restart-{}.bin",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, b"stale partial").unwrap();

        client
            .download_to_path(&format!("{base_url}/feed"), &path, false)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), body);
        std::fs::remove_file(path).unwrap();
    }

    async fn rate_limited_once(
        State((body, requests)): State<(Arc<Vec<u8>>, Arc<AtomicUsize>)>,
        request: Request<Body>,
    ) -> Response<Body> {
        if request.uri().path() == "/token" {
            return token_response();
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
    fn bike_identity_parser_rejects_invalid_scopes_and_parts() {
        use super::{odata_in_filter, tdx_bike_station_parts};
        use crate::domain::ids::{BikeStationId, bike_station_id};
        let valid = bike_station_id("Taichung", "TXG001");
        assert_eq!(tdx_bike_station_parts(&valid), Some(("Taichung", "TXG001")));
        for value in [
            "plain",
            "tdx:Taipei:",
            "tdx::TPE001",
            "tdx:Unknown:001",
            "tdx:Taipei:a:b",
            "tdx:Taipei: a",
            "tdx:Taipei:a\n",
        ] {
            assert!(
                tdx_bike_station_parts(&BikeStationId::new(value)).is_none(),
                "{value:?}"
            );
        }
        assert_eq!(
            odata_in_filter("StationUID", &["a'b".to_owned()]),
            "StationUID in ('a''b')"
        );
    }

    #[tokio::test]
    async fn retries_rate_limited_get_after_retry_after() {
        let expected = br#"{"available":true}"#.to_vec();
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(rate_limited_once))
            .with_state((Arc::new(expected.clone()), requests.clone()));
        let (client, _) = spawn_client(app).await;

        let response = client.get_bytes("availability", &[]).await.unwrap();

        assert_eq!(response.as_ref(), expected);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn request_time_reads_return_rate_limits_without_waiting() {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(rate_limited_once))
            .with_state((Arc::new(b"[]".to_vec()), requests.clone()));
        let (client, _) = spawn_client(app).await;

        let realtime = client.get_json::<serde_json::Value>("live", &[]).await;

        assert!(matches!(
            realtime,
            Err(super::TdxClientError::RateLimitedWithRetryAfter(_)
                | super::TdxClientError::RateLimited)
        ));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn streamed_download_exceeds_json_response_limit() {
        let expected_size = crate::config::ViaConfig::default().tdx.max_response_bytes + 1;
        let body = Arc::new(vec![0x5a; expected_size]);
        let app = Router::new().fallback(any(mock_response)).with_state(body);
        let (client, base_url) = spawn_client(app).await;
        let path = std::env::temp_dir().join(format!(
            "via-tdx-large-download-{}.bin",
            uuid::Uuid::new_v4().simple()
        ));

        let bytes_written = client
            .download_to_path(&format!("{base_url}/feed"), &path, false)
            .await
            .unwrap();
        assert_eq!(bytes_written, expected_size as u64);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), bytes_written);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn retry_after_is_capped_at_sixty_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "3600".parse().unwrap());
        assert_eq!(
            super::retry_after(&headers),
            Some(std::time::Duration::from_secs(60))
        );
        headers.insert("retry-after", "7".parse().unwrap());
        assert_eq!(
            super::retry_after(&headers),
            Some(std::time::Duration::from_secs(7))
        );
    }

    /// Answers 401 to the first API call, and counts token requests.
    async fn unauthorized_once(
        State((tokens, calls)): State<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
        request: Request<Body>,
    ) -> Response<Body> {
        if request.uri().path() == "/token" {
            tokens.fetch_add(1, Ordering::SeqCst);
            return token_response();
        }
        let status = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::OK
        };
        Response::builder()
            .status(status)
            .body(Body::from("{}"))
            .unwrap()
    }

    #[tokio::test]
    async fn a_401_drops_the_cached_token_and_retries_once() {
        let tokens = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(unauthorized_once))
            .with_state((tokens.clone(), calls.clone()));
        let (client, _) = spawn_client(app).await;

        client.get_bytes("live", &[]).await.unwrap();

        assert_eq!(tokens.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_second_401_is_an_error_not_a_loop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().fallback(any(move |request: Request<Body>| {
            let counted = counted.clone();
            async move {
                if request.uri().path() == "/token" {
                    return token_response();
                }
                counted.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Body::empty())
                    .unwrap()
            }
        }));
        let (client, _) = spawn_client(app).await;

        assert!(client.get_bytes("live", &[]).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
