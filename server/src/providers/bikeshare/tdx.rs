use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        bikeshare::{BikeAvailabilitySnapshot, BikeStationAvailability},
        ids::BikeStationId,
    },
    live::{
        BikeAvailabilityRecord, CachedLiveData, DirectLiveData, LiveBatch, LiveCacheSettings,
        LiveDataSource, UnavailableReason,
    },
    tdx::{TdxClient, bike_station_id, tdx_bike_station_parts},
};

use crate::domain::bikeshare::{BikeAvailabilityError, BikeAvailabilitySource};

#[derive(Clone)]
pub struct TdxBikeAvailabilitySource {
    live: Arc<dyn LiveDataSource>,
    settings: crate::tdx::BikeAvailabilitySettings,
}

impl TdxBikeAvailabilitySource {
    pub fn new(
        tdx_client: Arc<TdxClient>,
        batch_size: usize,
        settings: crate::tdx::BikeAvailabilitySettings,
    ) -> Self {
        let cache = LiveCacheSettings::from(&crate::tdx::TdxSettings {
            bikeshare: settings.clone(),
            ..tdx_client.settings.clone()
        });
        Self::with_live_data(
            Arc::new(CachedLiveData::new(
                Arc::new(DirectLiveData::new(tdx_client).with_bike_batch_size(batch_size)),
                cache,
            )),
            settings,
        )
    }

    pub fn with_live_data(
        live: Arc<dyn LiveDataSource>,
        settings: crate::tdx::BikeAvailabilitySettings,
    ) -> Self {
        Self { live, settings }
    }

    fn is_fresh(&self, availability: &BikeStationAvailability, now: OffsetDateTime) -> bool {
        availability.is_fresh_with_skew(
            now,
            time::Duration::seconds(i64::from(self.settings.max_age_seconds)),
            time::Duration::seconds(i64::from(self.settings.max_clock_skew_seconds)),
        )
    }

    async fn load_city(
        &self,
        city: &str,
        ids: Vec<BikeStationId>,
    ) -> (
        HashMap<BikeStationId, BikeStationAvailability>,
        Option<BikeAvailabilityError>,
    ) {
        let uids = ids
            .iter()
            .map(|id| {
                tdx_bike_station_parts(id)
                    .expect("validated TDX station ID")
                    .1
                    .to_owned()
            })
            .collect::<Vec<_>>();
        let batch = self.live.bike_availability(city, &uids).await;
        let mut failure = batch_failure(city, &batch);
        let received_at = OffsetDateTime::now_utc();
        let required_ids = ids.iter().collect::<HashSet<_>>();
        let record_count = batch.records.len();
        let mut availability_by_station = HashMap::new();
        let mut duplicated_ids = HashSet::new();
        for record in batch.records {
            let Some(availability) = normalize_record(city, record) else {
                continue;
            };
            if !self.is_fresh(&availability, received_at)
                || !required_ids.contains(&availability.station_id)
            {
                continue;
            }
            let station_id = availability.station_id.clone();
            if availability_by_station
                .insert(station_id.clone(), availability)
                .is_some()
            {
                duplicated_ids.insert(station_id);
            }
        }
        let rejected =
            !duplicated_ids.is_empty() || (record_count > 0 && availability_by_station.is_empty());
        availability_by_station.retain(|station_id, _| !duplicated_ids.contains(station_id));
        if rejected {
            tracing::warn!(
                target: "via_server::providers::bikeshare",
                provider = "tdx", city, operation = "bike_availability",
                outcome = "rejected_payload",
                "bike availability payload was rejected; retaining usable station records"
            );
            failure.get_or_insert(BikeAvailabilityError::RejectedPayload);
        }
        (availability_by_station, failure)
    }
}

fn batch_failure(
    city: &str,
    batch: &LiveBatch<BikeAvailabilityRecord>,
) -> Option<BikeAvailabilityError> {
    let error = availability_error(batch.unavailable.first()?.reason);
    tracing::warn!(
        target: "via_server::providers::bikeshare",
        provider = "tdx", city, operation = "bike_availability",
        outcome = bike_availability_outcome(&error), error = %error,
        stations = batch.unavailable.len(),
        "bike availability was unavailable for some stations; retaining successful station records"
    );
    Some(error)
}

#[async_trait]
impl BikeAvailabilitySource for TdxBikeAvailabilitySource {
    async fn load_snapshot(
        &self,
        station_ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        let required_ids = station_ids.iter().collect::<HashSet<_>>();
        ::metrics::counter!("via_bike_availability_stations_requested_total")
            .increment(required_ids.len() as u64);
        let mut station_ids_by_city = BTreeMap::<String, Vec<BikeStationId>>::new();
        for id in required_ids {
            let Some((city, _)) = tdx_bike_station_parts(id) else {
                tracing::warn!(target: "via_server::providers::bikeshare", station_id = %id.as_str(), "unsupported bike station identity; skipping availability");
                continue;
            };
            station_ids_by_city
                .entry(city.to_owned())
                .or_default()
                .push(id.clone());
        }
        for ids in station_ids_by_city.values_mut() {
            ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        }
        let mut results = stream::iter(
            station_ids_by_city
                .into_iter()
                .map(|(city, ids)| async move { self.load_city(&city, ids).await }),
        )
        .buffer_unordered(self.settings.max_concurrent_cities);
        let mut availability_by_station = HashMap::new();
        let mut failure = None;
        while let Some((loaded, error)) = results.next().await {
            availability_by_station.extend(loaded);
            if let Some(error) = error {
                failure.get_or_insert(error);
            }
        }
        let now = OffsetDateTime::now_utc();
        availability_by_station.retain(|_, availability| self.is_fresh(availability, now));
        if availability_by_station.is_empty()
            && let Some(error) = failure
        {
            return Err(error);
        }
        Ok(Arc::new(BikeAvailabilitySnapshot::new(
            now,
            availability_by_station,
        )))
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

fn normalize_record(city: &str, record: BikeAvailabilityRecord) -> Option<BikeStationAvailability> {
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

fn availability_error(reason: UnavailableReason) -> BikeAvailabilityError {
    match reason {
        UnavailableReason::RateLimited => BikeAvailabilityError::RateLimited,
        UnavailableReason::InvalidResponse => BikeAvailabilityError::RejectedPayload,
        UnavailableReason::InvalidConfiguration => BikeAvailabilityError::InvalidConfiguration,
        UnavailableReason::TimedOut | UnavailableReason::UpstreamUnavailable => {
            BikeAvailabilityError::TemporarilyUnavailable
        }
    }
}
