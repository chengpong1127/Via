use std::{
    collections::{HashMap, HashSet},
    future::Future,
    hash::Hash,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use moka::future::Cache;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    BikeAvailabilityRecord, BusArrival, LiveBatch, LiveDataSource, MetroBoard, MetroPair, TraDelay,
    UnavailableReason,
};
use crate::tdx::TdxSettings;

const ENTRY_BASE_WEIGHT: usize = 96;
const RECORD_WEIGHT: usize = 192;

#[derive(Clone, Debug)]
pub struct LiveCacheSettings {
    pub realtime_ttl: Duration,
    pub realtime_max_bytes: u64,
    pub bike_ttl: Duration,
    pub bike_capacity: u64,
    pub bike_max_age: Duration,
    pub rate_limit_backoff: Duration,
}

impl From<&TdxSettings> for LiveCacheSettings {
    fn from(settings: &TdxSettings) -> Self {
        Self {
            realtime_ttl: Duration::from_secs(settings.realtime.cache_ttl_seconds),
            realtime_max_bytes: settings.realtime.cache_max_bytes,
            bike_ttl: Duration::from_secs(settings.bikeshare.cache_ttl_seconds),
            bike_capacity: settings.bikeshare.cache_capacity,
            bike_max_age: Duration::from_secs(u64::from(settings.bikeshare.max_age_seconds)),
            rate_limit_backoff: Duration::from_secs(settings.rate_limit_backoff_seconds),
        }
    }
}

pub struct CachedLiveData {
    inner: Arc<dyn LiveDataSource>,
    bus: EntityCache<String, BusArrival>,
    bike: EntityCache<String, BikeAvailabilityRecord>,
    metro: EntityCache<MetroPair, MetroBoard>,
    tra: EntityCache<String, TraDelay>,
    blocked_until: Mutex<Option<Instant>>,
    rate_limit_backoff: Duration,
}

impl CachedLiveData {
    pub fn new(inner: Arc<dyn LiveDataSource>, settings: LiveCacheSettings) -> Self {
        let realtime_bytes = settings.realtime_max_bytes / 3;
        Self {
            inner,
            bus: EntityCache::weighted(
                "via_tdx_realtime_cache_hits_total",
                settings.realtime_ttl,
                realtime_bytes,
            ),
            metro: EntityCache::weighted(
                "via_tdx_realtime_cache_hits_total",
                settings.realtime_ttl,
                realtime_bytes,
            ),
            tra: EntityCache::weighted(
                "via_tdx_realtime_cache_hits_total",
                settings.realtime_ttl,
                realtime_bytes,
            ),
            bike: EntityCache::counted(
                "via_bike_availability_station_cache_hits_total",
                settings.bike_ttl,
                settings.bike_capacity,
            )
            .reusable_while(move |record: &BikeAvailabilityRecord| {
                bike_observation_is_current(record, settings.bike_max_age)
            }),
            blocked_until: Mutex::new(None),
            rate_limit_backoff: settings.rate_limit_backoff,
        }
    }

    fn blocked(&self) -> bool {
        let mut blocked_until = self.blocked_until.lock().expect("rate limit block lock");
        match *blocked_until {
            Some(until) if until > Instant::now() => true,
            Some(_) => {
                *blocked_until = None;
                false
            }
            None => false,
        }
    }

    fn block(&self, retry_after: Option<Duration>) {
        let duration = retry_after.unwrap_or(self.rate_limit_backoff);
        if duration.is_zero() {
            return;
        }
        let until = Instant::now() + duration;
        let mut blocked_until = self.blocked_until.lock().expect("rate limit block lock");
        if blocked_until.is_none_or(|current| current < until) {
            *blocked_until = Some(until);
        }
    }

    async fn load<K, T, F, Fut>(
        &self,
        cache: &EntityCache<K, T>,
        scope: &str,
        ids: &[K],
        key_of: impl Fn(&T) -> Option<K>,
        fetch: F,
    ) -> LiveBatch<T, K>
    where
        K: Clone + Eq + Hash + Ord + Send + Sync + 'static,
        T: Clone + Send + Sync + 'static,
        F: FnOnce(Vec<K>) -> Fut,
        Fut: Future<Output = LiveBatch<T, K>>,
    {
        let mut ids = ids.to_vec();
        ids.sort();
        ids.dedup();
        let mut batch = LiveBatch::default();
        let missing = cache.take_cached(scope, ids, &mut batch.records).await;
        if missing.is_empty() {
            return batch;
        }
        let refresh = cache.refresh_lock(scope);
        let _refresh = refresh.lock().await;
        let missing = cache.take_cached(scope, missing, &mut batch.records).await;
        if missing.is_empty() {
            return batch;
        }
        if self.blocked() {
            batch.mark_unavailable(&missing, UnavailableReason::RateLimited);
            return batch;
        }
        let fetched = fetch(missing.clone()).await;
        if fetched.is_rate_limited() {
            self.block(fetched.retry_after);
        }
        let failed = fetched
            .unavailable
            .iter()
            .map(|unavailable| &unavailable.id)
            .collect::<HashSet<_>>();
        let requested = missing.iter().collect::<HashSet<_>>();
        let mut records_by_id = HashMap::<K, Vec<T>>::new();
        for record in fetched.records {
            if let Some(key) = key_of(&record)
                && requested.contains(&key)
            {
                records_by_id.entry(key).or_default().push(record);
            }
        }
        for id in &missing {
            if failed.contains(id) {
                continue;
            }
            let records = Arc::new(records_by_id.remove(id).unwrap_or_default());
            batch.records.extend(records.iter().cloned());
            cache
                .entries
                .insert((scope.to_owned(), id.clone()), records)
                .await;
        }
        batch.unavailable = fetched.unavailable;
        batch.upstream_requests = fetched.upstream_requests;
        batch.retry_after = fetched.retry_after;
        batch
    }
}

#[async_trait]
impl LiveDataSource for CachedLiveData {
    async fn bus_arrivals(&self, scope: &str, stop_uids: &[String]) -> LiveBatch<BusArrival> {
        self.load(
            &self.bus,
            scope,
            stop_uids,
            |arrival| Some(arrival.stop_uid.clone()),
            |missing| async move { self.inner.bus_arrivals(scope, &missing).await },
        )
        .await
    }

    async fn bike_availability(
        &self,
        city: &str,
        station_uids: &[String],
    ) -> LiveBatch<BikeAvailabilityRecord> {
        self.load(
            &self.bike,
            city,
            station_uids,
            |record| {
                record
                    .station_uid
                    .as_deref()
                    .map(|station_uid| station_uid.trim().to_owned())
            },
            |missing| async move { self.inner.bike_availability(city, &missing).await },
        )
        .await
    }

    async fn metro_boards(&self, pairs: &[MetroPair]) -> LiveBatch<MetroBoard, MetroPair> {
        self.load(
            &self.metro,
            "",
            pairs,
            |board| {
                Some(MetroPair {
                    line_id: board.line_id.clone(),
                    station_id: board.station_id.clone(),
                })
            },
            |missing| async move { self.inner.metro_boards(&missing).await },
        )
        .await
    }

    async fn tra_delays(&self, train_numbers: &[String]) -> LiveBatch<TraDelay> {
        self.load(
            &self.tra,
            "",
            train_numbers,
            |delay| Some(delay.train_no.clone()),
            |missing| async move { self.inner.tra_delays(&missing).await },
        )
        .await
    }
}

type Reusable<T> = Box<dyn Fn(&T) -> bool + Send + Sync>;

struct EntityCache<K, T> {
    entries: Cache<(String, K), Arc<Vec<T>>>,
    refreshes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    hit_metric: &'static str,
    reusable: Option<Reusable<T>>,
}

impl<K, T> EntityCache<K, T>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
{
    fn weighted(hit_metric: &'static str, ttl: Duration, max_bytes: u64) -> Self {
        Self::build(
            hit_metric,
            Cache::builder()
                .max_capacity(max_bytes)
                .weigher(|_key, records: &Arc<Vec<T>>| {
                    u32::try_from(ENTRY_BASE_WEIGHT + records.len() * RECORD_WEIGHT)
                        .unwrap_or(u32::MAX)
                })
                .time_to_live(ttl)
                .build(),
        )
    }

    fn counted(hit_metric: &'static str, ttl: Duration, capacity: u64) -> Self {
        Self::build(
            hit_metric,
            Cache::builder()
                .max_capacity(capacity)
                .time_to_live(ttl)
                .build(),
        )
    }

    fn build(hit_metric: &'static str, entries: Cache<(String, K), Arc<Vec<T>>>) -> Self {
        Self {
            entries,
            refreshes: Mutex::new(HashMap::new()),
            hit_metric,
            reusable: None,
        }
    }

    fn reusable_while(mut self, reusable: impl Fn(&T) -> bool + Send + Sync + 'static) -> Self {
        self.reusable = Some(Box::new(reusable));
        self
    }

    async fn take_cached(&self, scope: &str, ids: Vec<K>, records: &mut Vec<T>) -> Vec<K> {
        let mut missing = Vec::new();
        let mut hits = 0_u64;
        for id in ids {
            let cached = self
                .entries
                .get(&(scope.to_owned(), id.clone()))
                .await
                .filter(|cached| {
                    self.reusable
                        .as_ref()
                        .is_none_or(|reusable| cached.iter().all(reusable))
                });
            match cached {
                Some(cached) => {
                    hits += 1;
                    records.extend(cached.iter().cloned());
                }
                None => missing.push(id),
            }
        }
        if hits > 0 {
            ::metrics::counter!(self.hit_metric).increment(hits);
        }
        missing
    }

    fn refresh_lock(&self, scope: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.refreshes
            .lock()
            .expect("live cache refresh lock")
            .entry(scope.to_owned())
            .or_default()
            .clone()
    }
}

fn bike_observation_is_current(record: &BikeAvailabilityRecord, max_age: Duration) -> bool {
    let Some(observed_at) = record
        .source_update_time
        .as_deref()
        .or(record.update_time.as_deref())
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
    else {
        return true;
    };
    OffsetDateTime::now_utc() - observed_at <= max_age
}
