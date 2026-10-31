use std::{
    fmt,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use moka::future::Cache;
use reqwest::{Client, Response, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

const DIRECTIONAL_SERVICE_SEPARATOR: &str = "::direction=";

pub fn boarding_point_id(stop_uid: impl Into<String>) -> crate::domain::ids::BoardingPointId {
    crate::domain::ids::BoardingPointId::new(stop_uid)
}

pub fn bike_station_id(
    city: impl Into<String>,
    station_uid: impl Into<String>,
) -> crate::domain::ids::BikeStationId {
    crate::domain::ids::BikeStationId::new(format!("tdx:{}:{}", city.into(), station_uid.into()))
}

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

pub fn tdx_bike_station_parts(id: &crate::domain::ids::BikeStationId) -> Option<(&str, &str)> {
    let (city, uid) = id.as_str().strip_prefix("tdx:")?.split_once(':')?;
    if !BIKE_CITIES.iter().any(|(scope, _)| *scope == city)
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

pub fn transit_service_id(
    sub_route_uid: impl Into<String>,
) -> crate::domain::ids::TransitPatternId {
    crate::domain::ids::TransitPatternId::new(sub_route_uid)
}

pub fn transit_service_id_for_direction(
    sub_route_uid: impl Into<String>,
    direction: u8,
) -> crate::domain::ids::TransitPatternId {
    crate::domain::ids::TransitPatternId::new(format!(
        "{}{DIRECTIONAL_SERVICE_SEPARATOR}{direction}",
        sub_route_uid.into()
    ))
}

pub fn tdx_sub_route_uid(pattern_id: &crate::domain::ids::TransitPatternId) -> &str {
    pattern_id
        .as_str()
        .split_once(DIRECTIONAL_SERVICE_SEPARATOR)
        .map_or_else(|| pattern_id.as_str(), |(sub_route_uid, _)| sub_route_uid)
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
    download_client: Client,
    credentials: TdxCredentials,
    api_base_url: Url,
    token_url: Url,
    token: Arc<Mutex<Option<CachedToken>>>,
    realtime_responses: Cache<String, bytes::Bytes>,
    pub(crate) settings: TdxSettings,
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
    pub fn new(credentials: TdxCredentials, settings: TdxSettings) -> Result<Self, TdxClientError> {
        let api_base_url =
            Url::parse(&settings.api_base_url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        let token_url =
            Url::parse(&settings.token_url).map_err(|_| TdxClientError::InvalidConfiguration)?;
        if api_base_url.host_str().is_none() || token_url.host_str().is_none() {
            return Err(TdxClientError::InvalidConfiguration);
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(settings.connect_timeout_ms))
            .timeout(Duration::from_millis(settings.request_timeout_ms))
            .user_agent("via-data/1.0")
            .build()
            .map_err(TdxClientError::ApiRequest)?;
        let download_client = Client::builder()
            .connect_timeout(Duration::from_millis(settings.connect_timeout_ms))
            .timeout(Duration::from_secs(settings.download_timeout_seconds))
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
            realtime_responses: Cache::builder()
                .max_capacity(settings.realtime.cache_max_bytes)
                .weigher(|_url, body: &bytes::Bytes| u32::try_from(body.len()).unwrap_or(u32::MAX))
                .time_to_live(Duration::from_secs(settings.realtime.cache_ttl_seconds))
                .build(),
            settings,
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
                    && retries < self.settings.max_rate_limit_retries
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
        self.get_url_bytes(self.api_url(path, query)?).await
    }

    async fn get_url_bytes(&self, url: Url) -> Result<bytes::Bytes, TdxClientError> {
        let response = self.authenticated_get(url).await?;
        if response
            .content_length()
            .is_some_and(|length| length > self.settings.max_response_bytes as u64)
        {
            return Err(TdxClientError::ResponseTooLarge);
        }
        let body = response.bytes().await.map_err(TdxClientError::ApiRequest)?;
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
        let body = self.get_bytes(path, query).await?;
        serde_json::from_slice(&body).map_err(TdxClientError::InvalidJson)
    }

    pub async fn get_realtime_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, TdxClientError> {
        let url = self.api_url(path, query)?;
        let key = url.as_str().to_owned();
        let body = if let Some(body) = self.realtime_responses.get(&key).await {
            ::metrics::counter!("via_tdx_realtime_cache_hits_total").increment(1);
            body
        } else {
            let body = self.get_url_bytes(url).await?;
            self.realtime_responses.insert(key, body.clone()).await;
            body
        };
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
    pub batch: TdxBatchSettings,
    pub realtime: RealtimeSettings,
    pub bikeshare: BikeAvailabilitySettings,
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
    fn bike_identity_parser_rejects_invalid_scopes_and_parts() {
        use super::{bike_station_id, odata_in_filter, tdx_bike_station_parts};
        use crate::domain::ids::BikeStationId;
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

        let response = client.get_bytes("availability", &[]).await.unwrap();

        assert_eq!(response.as_ref(), expected);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    async fn counted_response(
        State(requests): State<Arc<AtomicUsize>>,
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
        let count = requests.fetch_add(1, Ordering::SeqCst) + 1;
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(format!("{count}")))
            .unwrap()
    }

    #[tokio::test]
    async fn realtime_responses_are_reused_for_the_same_url_only() {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(counted_response))
            .with_state(requests.clone());
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
        let stop_a = [("$filter", "StopUID in ('A')")];
        let stop_b = [("$filter", "StopUID in ('B')")];

        let first: u32 = client.get_realtime_json("live", &stop_a).await.unwrap();
        let repeated: u32 = client.get_realtime_json("live", &stop_a).await.unwrap();
        let other: u32 = client.get_realtime_json("live", &stop_b).await.unwrap();
        let uncached: u32 = client.get_json("live", &stop_a).await.unwrap();

        assert_eq!((first, repeated, other, uncached), (1, 1, 2, 3));
        assert_eq!(requests.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn streamed_download_exceeds_json_response_limit() {
        let expected_size = crate::config::ViaConfig::default().tdx.max_response_bytes + 1;
        let body = Arc::new(vec![0x5a; expected_size]);
        let app = Router::new().fallback(any(mock_response)).with_state(body);
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
