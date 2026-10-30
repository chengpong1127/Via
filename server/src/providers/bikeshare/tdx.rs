use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use moka::future::Cache;
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex;

use crate::{
    domain::{
        bikeshare::{BikeAvailabilitySnapshot, BikeStationAvailability},
        ids::BikeStationId,
    },
    tdx::{
        BIKE_CITIES, TdxClient, TdxClientError, bike_station_id, odata_in_filter,
        tdx_bike_station_parts,
    },
};

use crate::domain::bikeshare::{BikeAvailabilityError, BikeAvailabilitySource};

#[derive(Clone)]
pub struct TdxBikeAvailabilitySource {
    tdx_client: Arc<TdxClient>,
    batch_size: usize,
    settings: crate::tdx::BikeAvailabilitySettings,
    station_cache: Cache<BikeStationId, BikeStationAvailability>,
    city_refreshes: Arc<HashMap<&'static str, Mutex<()>>>,
}

impl TdxBikeAvailabilitySource {
    pub fn new(
        tdx_client: Arc<TdxClient>,
        batch_size: usize,
        settings: crate::tdx::BikeAvailabilitySettings,
    ) -> Self {
        Self {
            tdx_client,
            batch_size,
            station_cache: Cache::builder()
                .max_capacity(settings.cache_capacity)
                .time_to_live(Duration::from_secs(settings.cache_ttl_seconds))
                .build(),
            settings,
            city_refreshes: Arc::new(
                BIKE_CITIES
                    .iter()
                    .map(|(city, _)| (*city, Mutex::new(())))
                    .collect(),
            ),
        }
    }

    async fn fresh_cached(&self, id: &BikeStationId) -> Option<BikeStationAvailability> {
        self.station_cache.get(id).await.filter(|availability| {
            availability.is_fresh_with_skew(
                OffsetDateTime::now_utc(),
                time::Duration::seconds(i64::from(self.settings.max_age_seconds)),
                time::Duration::seconds(i64::from(self.settings.max_clock_skew_seconds)),
            )
        })
    }

    async fn load_city(
        &self,
        city: &str,
        ids: Vec<BikeStationId>,
    ) -> (
        HashMap<BikeStationId, BikeStationAvailability>,
        Option<BikeAvailabilityError>,
    ) {
        let _refresh = self.city_refreshes[city].lock().await;
        let mut stations = HashMap::new();
        let mut missing = Vec::new();
        for id in ids {
            if let Some(availability) = self.fresh_cached(&id).await {
                stations.insert(id, availability);
            } else {
                missing.push(id);
            }
        }
        let mut failure = None;
        for batch in missing.chunks(self.batch_size) {
            match self.fetch_batch(city, batch).await {
                Ok(records) => {
                    for (id, availability) in records {
                        self.station_cache
                            .insert(id.clone(), availability.clone())
                            .await;
                        stations.insert(id, availability);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: "via_server::providers::bikeshare",
                        provider = "tdx", city, operation = "bike_availability",
                        outcome = bike_availability_outcome(&error), error = %error,
                        "bike availability batch failed; retaining successful station records"
                    );
                    failure.get_or_insert(error);
                }
            }
        }
        (stations, failure)
    }

    async fn fetch_batch(
        &self,
        city: &str,
        ids: &[BikeStationId],
    ) -> Result<HashMap<BikeStationId, BikeStationAvailability>, BikeAvailabilityError> {
        let uids = ids
            .iter()
            .map(|id| {
                tdx_bike_station_parts(id)
                    .expect("validated TDX station ID")
                    .1
                    .to_owned()
            })
            .collect::<Vec<_>>();
        let filter = odata_in_filter("StationUID", &uids);
        let top = ids.len().to_string();
        let path = format!("v2/Bike/Availability/City/{city}");
        ::metrics::counter!("via_bike_availability_http_requests_total").increment(1);
        ::metrics::counter!("via_bike_availability_stations_fetched_total")
            .increment(ids.len() as u64);
        let records = self
            .tdx_client
            .get_json::<Vec<TdxBikeAvailabilityRecord>>(
                &path,
                &[("$format", "JSON"), ("$filter", &filter), ("$top", &top)],
            )
            .await
            .map_err(map_tdx_error)?;
        let received_at = OffsetDateTime::now_utc();
        let required = ids.iter().collect::<HashSet<_>>();
        let record_count = records.len();
        let mut stations = HashMap::new();
        for record in records {
            let Some(availability) = normalize_record(city, record, received_at, &self.settings)
            else {
                continue;
            };
            if !required.contains(&availability.station_id) {
                continue;
            }
            if stations
                .insert(availability.station_id.clone(), availability)
                .is_some()
            {
                return Err(BikeAvailabilityError::RejectedPayload);
            }
        }
        if record_count > 0 && stations.is_empty() {
            return Err(BikeAvailabilityError::RejectedPayload);
        }
        Ok(stations)
    }
}

#[async_trait]
impl BikeAvailabilitySource for TdxBikeAvailabilitySource {
    async fn snapshot(
        &self,
        station_ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        let required = station_ids.iter().collect::<HashSet<_>>();
        ::metrics::counter!("via_bike_availability_stations_requested_total")
            .increment(required.len() as u64);
        let mut stations = HashMap::new();
        let mut cities = BTreeMap::<String, Vec<BikeStationId>>::new();
        for id in required {
            let Some((city, _)) = tdx_bike_station_parts(id) else {
                tracing::warn!(target: "via_server::providers::bikeshare", station_id = %id.as_str(), "unsupported bike station identity; skipping availability");
                continue;
            };
            if let Some(availability) = self.fresh_cached(id).await {
                ::metrics::counter!("via_bike_availability_station_cache_hits_total").increment(1);
                stations.insert(id.clone(), availability);
            } else {
                cities.entry(city.to_owned()).or_default().push(id.clone());
            }
        }
        for ids in cities.values_mut() {
            ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        }
        let mut results = stream::iter(
            cities
                .into_iter()
                .map(|(city, ids)| async move { self.load_city(&city, ids).await }),
        )
        .buffer_unordered(self.settings.max_concurrent_cities);
        let mut failure = None;
        while let Some((loaded, error)) = results.next().await {
            stations.extend(loaded);
            if let Some(error) = error {
                failure.get_or_insert(error);
            }
        }
        let now = OffsetDateTime::now_utc();
        stations.retain(|_, availability| {
            availability.is_fresh_with_skew(
                now,
                time::Duration::seconds(i64::from(self.settings.max_age_seconds)),
                time::Duration::seconds(i64::from(self.settings.max_clock_skew_seconds)),
            )
        });
        if stations.is_empty()
            && let Some(error) = failure
        {
            return Err(error);
        }
        Ok(Arc::new(BikeAvailabilitySnapshot::new(now, stations)))
    }
}

fn bike_availability_outcome(error: &BikeAvailabilityError) -> &'static str {
    match error {
        BikeAvailabilityError::RejectedPayload => "rejected_payload",
        BikeAvailabilityError::TemporarilyUnavailable => "unavailable",
        BikeAvailabilityError::RateLimited => "rate_limited",
        BikeAvailabilityError::InvalidConfiguration => "invalid_configuration",
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
    settings: &crate::tdx::BikeAvailabilitySettings,
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
    let availability = BikeStationAvailability::new(
        bike_station_id(city, station_uid),
        available_bikes,
        available_return_slots,
        in_service,
        observed_at,
    );
    availability
        .is_fresh_with_skew(
            received_at,
            time::Duration::seconds(i64::from(settings.max_age_seconds)),
            time::Duration::seconds(i64::from(settings.max_clock_skew_seconds)),
        )
        .then_some(availability)
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
        | TdxClientError::DownloadWrite(_)
        | TdxClientError::ResponseTooLarge => BikeAvailabilityError::TemporarilyUnavailable,
    }
}
