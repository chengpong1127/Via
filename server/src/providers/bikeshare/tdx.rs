//! TDX Bike Availability adapter.
//!
//! TDX field names stop at this module. The rest of the application receives
//! a provider-neutral, static-network-matched snapshot.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::future::try_join_all;
use moka::future::Cache;
use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::bikeshare::{
        BikeAvailabilitySnapshot, BikeShareNetwork, BikeStationAvailability,
        DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
    },
    tdx::{TdxClient, TdxClientError, bike_station_id},
};

use super::{BikeAvailabilityError, BikeAvailabilitySource, DEFAULT_BIKE_AVAILABILITY_CACHE_TTL};

const SUPPORTED_CITIES: [(&str, &str); 2] = [("Taipei", "Taipei"), ("NewTaipei", "NewTaipei")];
const MAX_PROVIDER_CLOCK_SKEW: TimeDuration = TimeDuration::seconds(300);

#[derive(Clone)]
pub struct TdxBikeAvailabilitySource {
    network: Arc<BikeShareNetwork>,
    tdx_client: Arc<TdxClient>,
    snapshot_cache: Cache<(), Arc<BikeAvailabilitySnapshot>>,
    cache_ttl: Duration,
    max_observation_age: TimeDuration,
}

impl TdxBikeAvailabilitySource {
    pub fn new(network: Arc<BikeShareNetwork>, tdx_client: Arc<TdxClient>) -> Self {
        Self::with_cache_ttl(network, tdx_client, DEFAULT_BIKE_AVAILABILITY_CACHE_TTL)
    }

    pub fn with_cache_ttl(
        network: Arc<BikeShareNetwork>,
        tdx_client: Arc<TdxClient>,
        cache_ttl: Duration,
    ) -> Self {
        let cache_ttl = cache_ttl.max(Duration::from_millis(1));
        Self {
            network,
            tdx_client,
            snapshot_cache: Cache::builder().time_to_live(cache_ttl).build(),
            cache_ttl,
            max_observation_age: DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
        }
    }

    pub fn with_max_observation_age(mut self, max_observation_age: TimeDuration) -> Self {
        self.max_observation_age = max_observation_age;
        self
    }

    pub fn cache_ttl(&self) -> Duration {
        self.cache_ttl
    }

    pub async fn clear_cache(&self) {
        self.snapshot_cache.invalidate_all();
    }

    async fn fetch_snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        let received_at = OffsetDateTime::now_utc();
        let city_payloads = try_join_all(
            SUPPORTED_CITIES
                .into_iter()
                .map(|(city, path_city)| self.fetch_city(city, path_city)),
        )
        .await?;

        let mut stations = HashMap::new();
        let mut record_count = 0;
        let mut valid_count = 0;
        for (city, records) in city_payloads {
            record_count += records.len();
            for record in records {
                let Some(availability) =
                    normalize_record(city, record, received_at, self.max_observation_age)
                else {
                    continue;
                };
                if self.network.station(&availability.station_id).is_none() {
                    continue;
                }
                if stations
                    .insert(availability.station_id.clone(), availability)
                    .is_some()
                {
                    return Err(BikeAvailabilityError::RejectedPayload);
                }
                valid_count += 1;
            }
        }
        if record_count > 0 && valid_count == 0 {
            return Err(BikeAvailabilityError::RejectedPayload);
        }
        Ok(Arc::new(BikeAvailabilitySnapshot::new(
            received_at,
            stations,
        )))
    }

    async fn fetch_city(
        &self,
        city: &'static str,
        path_city: &'static str,
    ) -> Result<(&'static str, Vec<TdxBikeAvailabilityRecord>), BikeAvailabilityError> {
        let path = format!("v2/Bike/Availability/City/{path_city}");
        let records = self
            .tdx_client
            .get_json::<Vec<TdxBikeAvailabilityRecord>>(&path, &[("$format", "JSON")])
            .await
            .map_err(map_tdx_error)?;
        Ok((city, records))
    }
}

#[async_trait]
impl BikeAvailabilitySource for TdxBikeAvailabilitySource {
    async fn snapshot(&self) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        let source = self.clone();
        self.snapshot_cache
            .try_get_with((), async move { source.fetch_snapshot().await })
            .await
            .map_err(|error| (*error).clone())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct TdxBikeAvailabilityRecord {
    #[serde(rename = "StationUID")]
    station_uid: Option<String>,
    #[serde(rename = "ServiceStatus")]
    service_status: Option<i32>,
    #[serde(rename = "AvailableRentBikes")]
    available_rent_bikes: Option<i64>,
    #[serde(rename = "AvailableReturnBikes")]
    available_return_bikes: Option<i64>,
    #[serde(rename = "SrcUpdateTime")]
    source_update_time: Option<String>,
    #[serde(rename = "UpdateTime")]
    update_time: Option<String>,
}

fn normalize_record(
    city: &str,
    record: TdxBikeAvailabilityRecord,
    received_at: OffsetDateTime,
    max_observation_age: TimeDuration,
) -> Option<BikeStationAvailability> {
    let station_uid = record.station_uid?.trim().to_owned();
    if station_uid.is_empty() {
        return None;
    }
    let available_bikes = nonnegative_count(record.available_rent_bikes?)?;
    let available_return_slots = nonnegative_count(record.available_return_bikes?)?;
    let in_service = match record.service_status? {
        1 => true,
        0 | 2 => false,
        _ => return None,
    };
    let update_time = parse_timestamp(record.update_time?.as_str())?;
    let observed_at = match record.source_update_time.as_deref() {
        Some(value) => parse_timestamp(value)?,
        None => update_time,
    };
    let age = received_at - observed_at;
    if age < TimeDuration::ZERO
        || age > max_observation_age
        || observed_at > received_at + MAX_PROVIDER_CLOCK_SKEW
    {
        return None;
    }
    Some(BikeStationAvailability::new(
        bike_station_id(city, station_uid),
        available_bikes,
        available_return_slots,
        in_service,
        observed_at,
    ))
}

fn nonnegative_count(value: i64) -> Option<u32> {
    u32::try_from(value).ok()
}

fn parse_timestamp(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn map_tdx_error(error: TdxClientError) -> BikeAvailabilityError {
    match error {
        TdxClientError::RateLimited | TdxClientError::RateLimitedWithRetryAfter(_) => {
            BikeAvailabilityError::RateLimited
        }
        TdxClientError::InvalidJson(_) => BikeAvailabilityError::RejectedPayload,
        TdxClientError::InvalidConfiguration => BikeAvailabilityError::InvalidConfiguration,
        TdxClientError::Credentials(_)
        | TdxClientError::OAuthRequest(_)
        | TdxClientError::InvalidOAuthResponse(_)
        | TdxClientError::ApiRequest(_)
        | TdxClientError::ResponseTooLarge => BikeAvailabilityError::TemporarilyUnavailable,
    }
}
