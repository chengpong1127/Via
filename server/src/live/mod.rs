mod cache;
mod client;
mod direct;
pub mod gateway;

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use cache::CachedLiveData;
pub use client::{GatewayClientError, GatewayLiveData};
pub use direct::DirectLiveData;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    RateLimited,
    TimedOut,
    UpstreamUnavailable,
    InvalidResponse,
    InvalidConfiguration,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::TimedOut => "timed_out",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::InvalidResponse => "invalid_response",
            Self::InvalidConfiguration => "invalid_configuration",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Unavailable<K> {
    pub id: K,
    pub reason: UnavailableReason,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LiveBatch<T, K = String> {
    pub records: Vec<T>,
    pub unavailable: Vec<Unavailable<K>>,
    #[serde(default)]
    pub upstream_requests: usize,
    #[serde(skip)]
    pub retry_after: Option<Duration>,
}

impl<T, K> Default for LiveBatch<T, K> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            unavailable: Vec::new(),
            upstream_requests: 0,
            retry_after: None,
        }
    }
}

impl<T, K: Clone> LiveBatch<T, K> {
    pub fn unavailable_all(ids: &[K], reason: UnavailableReason) -> Self {
        let mut batch = Self::default();
        batch.mark_unavailable(ids, reason);
        batch
    }

    pub fn mark_unavailable(&mut self, ids: &[K], reason: UnavailableReason) {
        self.unavailable.extend(ids.iter().map(|id| Unavailable {
            id: id.clone(),
            reason,
        }));
    }

    pub fn is_rate_limited(&self) -> bool {
        self.unavailable
            .iter()
            .any(|unavailable| unavailable.reason == UnavailableReason::RateLimited)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BusArrival {
    #[serde(alias = "StopUID")]
    pub stop_uid: String,
    #[serde(alias = "SubRouteUID", default)]
    pub sub_route_uid: Option<String>,
    #[serde(alias = "RouteUID", default)]
    pub route_uid: Option<String>,
    #[serde(alias = "Direction", default)]
    pub direction: Option<u8>,
    #[serde(alias = "StopStatus", default)]
    pub stop_status: Option<u8>,
    #[serde(alias = "EstimateTime", default)]
    pub estimate_seconds: Option<i64>,
    #[serde(alias = "UpdateTime")]
    pub update_time: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BikeAvailabilityRecord {
    #[serde(alias = "StationUID", default)]
    pub station_uid: Option<String>,
    #[serde(alias = "ServiceStatus", default)]
    pub service_status: Option<i32>,
    #[serde(alias = "AvailableRentBikes", default)]
    pub available_rent_bikes: Option<i64>,
    #[serde(alias = "AvailableReturnBikes", default)]
    pub available_return_bikes: Option<i64>,
    #[serde(alias = "SrcUpdateTime", default)]
    pub source_update_time: Option<String>,
    #[serde(alias = "UpdateTime", default)]
    pub update_time: Option<String>,
}

impl BikeAvailabilityRecord {
    pub fn station_key(&self) -> Option<&str> {
        self.station_uid.as_deref().map(str::trim)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MetroPair {
    pub line_id: String,
    pub station_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MetroBoard {
    pub line_id: String,
    pub station_id: String,
    pub destination_station_id: String,
    pub service_status: i32,
    pub estimate_minutes: Option<i64>,
    pub source_update_time: String,
    pub update_time: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TraDelay {
    pub train_no: String,
    pub delay_minutes: i64,
    pub update_time: String,
    pub board_update_time: String,
    pub board_source_update_time: String,
}

#[async_trait]
pub trait LiveDataSource: Send + Sync {
    async fn bus_arrivals(&self, scope: &str, stop_uids: &[String]) -> LiveBatch<BusArrival>;

    async fn bike_availability(
        &self,
        city: &str,
        station_uids: &[String],
    ) -> LiveBatch<BikeAvailabilityRecord>;

    async fn metro_boards(&self, pairs: &[MetroPair]) -> LiveBatch<MetroBoard, MetroPair>;

    async fn tra_delays(&self, train_numbers: &[String]) -> LiveBatch<TraDelay>;
}
