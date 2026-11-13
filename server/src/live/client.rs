use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use url::Url;

use super::{
    BikeAvailabilityRecord, BusArrival, LiveBatch, LiveDataSource, MetroBoard, MetroPair, TraDelay,
    UnavailableReason,
    gateway::{
        BIKE_AVAILABILITY_PATH, BUS_ARRIVALS_PATH, LiveRequest, METRO_BOARDS_PATH, TRA_DELAYS_PATH,
    },
};
use crate::tdx::TdxSettings;

#[derive(Debug, Error)]
pub enum GatewayClientError {
    #[error("tdx.gateway.url is not a valid base URL")]
    InvalidUrl,
    #[error("TDX gateway HTTP client could not be built")]
    Client(#[source] reqwest::Error),
}

#[derive(Clone)]
pub struct GatewayLiveData {
    client: Client,
    base_url: Url,
    max_response_bytes: usize,
}

impl GatewayLiveData {
    pub fn new(base_url: &str, settings: &TdxSettings) -> Result<Self, GatewayClientError> {
        let mut base_url = Url::parse(base_url).map_err(|_| GatewayClientError::InvalidUrl)?;
        if base_url.host_str().is_none() {
            return Err(GatewayClientError::InvalidUrl);
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(settings.connect_timeout_ms))
            .timeout(Duration::from_millis(
                settings
                    .request_timeout_ms
                    .saturating_add(settings.connect_timeout_ms),
            ))
            .user_agent("via-server/1.0")
            .build()
            .map_err(GatewayClientError::Client)?;
        Ok(Self {
            client,
            base_url,
            max_response_bytes: settings.max_response_bytes,
        })
    }

    async fn post<T, K>(&self, path: &str, scope: &str, ids: &[K]) -> LiveBatch<T, K>
    where
        T: DeserializeOwned,
        K: Clone + Serialize + DeserializeOwned,
    {
        if ids.is_empty() {
            return LiveBatch::default();
        }
        match self.request(path, scope, ids).await {
            Ok(batch) => batch,
            Err(reason) => {
                ::metrics::counter!("via_tdx_gateway_request_failures_total").increment(1);
                LiveBatch::unavailable_all(ids, reason)
            }
        }
    }

    async fn request<T, K>(
        &self,
        path: &str,
        scope: &str,
        ids: &[K],
    ) -> Result<LiveBatch<T, K>, UnavailableReason>
    where
        T: DeserializeOwned,
        K: Clone + Serialize + DeserializeOwned,
    {
        let url = self
            .base_url
            .join(path)
            .map_err(|_| UnavailableReason::InvalidConfiguration)?;
        let response = self
            .client
            .post(url)
            .json(&LiveRequest {
                scope: scope.to_owned(),
                ids: ids.to_vec(),
            })
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(transport_reason)?;
        let body = response.bytes().await.map_err(transport_reason)?;
        if body.len() > self.max_response_bytes {
            return Err(UnavailableReason::InvalidResponse);
        }
        serde_json::from_slice(&body).map_err(|_| UnavailableReason::InvalidResponse)
    }
}

fn transport_reason(error: reqwest::Error) -> UnavailableReason {
    if error.is_timeout() {
        UnavailableReason::TimedOut
    } else {
        UnavailableReason::UpstreamUnavailable
    }
}

#[async_trait]
impl LiveDataSource for GatewayLiveData {
    async fn bus_arrivals(&self, scope: &str, stop_uids: &[String]) -> LiveBatch<BusArrival> {
        self.post(BUS_ARRIVALS_PATH, scope, stop_uids).await
    }

    async fn bike_availability(
        &self,
        city: &str,
        station_uids: &[String],
    ) -> LiveBatch<BikeAvailabilityRecord> {
        self.post(BIKE_AVAILABILITY_PATH, city, station_uids).await
    }

    async fn metro_boards(&self, pairs: &[MetroPair]) -> LiveBatch<MetroBoard, MetroPair> {
        self.post(METRO_BOARDS_PATH, "", pairs).await
    }

    async fn tra_delays(&self, train_numbers: &[String]) -> LiveBatch<TraDelay> {
        self.post(TRA_DELAYS_PATH, "", train_numbers).await
    }
}
