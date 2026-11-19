use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::json;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::live::{
    BusArrival, CachedLiveData, DirectLiveData, LiveBatch, LiveDataSource, UnavailableReason,
};

use crate::support::tdx::spawn_tdx_fixture;

#[derive(Clone, Default)]
pub struct Upstream {
    pub calls: Arc<AtomicUsize>,
    pub filters: Arc<Mutex<Vec<String>>>,
    pub limited: Arc<AtomicBool>,
    pub retry_after: Option<&'static str>,
}

impl Upstream {
    fn requested_stops(&self) -> Vec<Vec<String>> {
        self.filters
            .lock()
            .unwrap()
            .iter()
            .map(|filter| quoted(filter))
            .collect()
    }
}

fn quoted(filter: &str) -> Vec<String> {
    filter
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

async fn respond(
    State(upstream): State<Upstream>,
    method: Method,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if method == Method::POST {
        return Json(json!({"access_token": "fixture-token", "expires_in": 300})).into_response();
    }
    upstream.calls.fetch_add(1, Ordering::SeqCst);
    let filter = query.get("$filter").cloned().unwrap_or_default();
    upstream.filters.lock().unwrap().push(filter.clone());
    if upstream.limited.load(Ordering::SeqCst) {
        let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
        if let Some(retry_after) = upstream.retry_after {
            response
                .headers_mut()
                .insert("retry-after", retry_after.parse().unwrap());
        }
        return response;
    }
    let update_time = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
    Json(
        quoted(&filter)
            .into_iter()
            .filter(|stop| !stop.starts_with("quiet"))
            .map(|stop| {
                json!({
                    "StopUID": stop,
                    "SubRouteUID": "service",
                    "Direction": 0,
                    "StopStatus": 0,
                    "EstimateTime": 60,
                    "UpdateTime": update_time,
                })
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

pub async fn cached_live_data(upstream: Upstream) -> Arc<CachedLiveData> {
    let app = Router::new().fallback(any(respond)).with_state(upstream);
    let tdx = spawn_tdx_fixture(app).await;
    let cache = tdx.settings().clone();
    Arc::new(CachedLiveData::new(
        Arc::new(DirectLiveData::new(tdx)),
        &cache,
    ))
}

pub fn stops(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

fn returned_stops(batch: &LiveBatch<BusArrival>) -> Vec<&str> {
    let mut stops = batch
        .records
        .iter()
        .map(|arrival| arrival.stop_uid.as_str())
        .collect::<Vec<_>>();
    stops.sort();
    stops
}

fn unavailable_stops(batch: &LiveBatch<BusArrival>) -> Vec<(&str, UnavailableReason)> {
    batch
        .unavailable
        .iter()
        .map(|unavailable| (unavailable.id.as_str(), unavailable.reason))
        .collect()
}

#[tokio::test]
async fn a_partial_cache_hit_requests_only_the_missing_stops() {
    let upstream = Upstream::default();
    let live = cached_live_data(upstream.clone()).await;

    let first = live.bus_arrivals("Taipei", &stops(&["1", "2", "3"])).await;
    let second = live.bus_arrivals("Taipei", &stops(&["2", "7", "9"])).await;

    assert_eq!(returned_stops(&first), ["1", "2", "3"]);
    assert_eq!(returned_stops(&second), ["2", "7", "9"]);
    assert_eq!((first.upstream_requests, second.upstream_requests), (1, 1));
    assert_eq!(
        upstream.requested_stops(),
        [stops(&["1", "2", "3"]), stops(&["7", "9"])]
    );
}

#[tokio::test]
async fn fully_cached_stops_and_stops_without_arrivals_make_no_upstream_call() {
    let upstream = Upstream::default();
    let live = cached_live_data(upstream.clone()).await;

    live.bus_arrivals("Taipei", &stops(&["1", "quiet-4"])).await;
    let repeated = live.bus_arrivals("Taipei", &stops(&["quiet-4", "1"])).await;

    assert_eq!(returned_stops(&repeated), ["1"]);
    assert!(repeated.unavailable.is_empty());
    assert_eq!(repeated.upstream_requests, 0);
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_same_stop_is_cached_separately_for_each_scope() {
    let upstream = Upstream::default();
    let live = cached_live_data(upstream.clone()).await;

    live.bus_arrivals("Taipei", &stops(&["1"])).await;
    live.bus_arrivals("NewTaipei", &stops(&["1"])).await;

    assert_eq!(upstream.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_rate_limit_keeps_cached_stops_and_stops_calling_upstream() {
    let upstream = Upstream {
        retry_after: Some("60"),
        ..Upstream::default()
    };
    let live = cached_live_data(upstream.clone()).await;
    live.bus_arrivals("Taipei", &stops(&["2"])).await;
    upstream.limited.store(true, Ordering::SeqCst);

    let limited = live.bus_arrivals("Taipei", &stops(&["2", "7", "9"])).await;

    assert_eq!(returned_stops(&limited), ["2"]);
    assert_eq!(
        unavailable_stops(&limited),
        [
            ("7", UnavailableReason::RateLimited),
            ("9", UnavailableReason::RateLimited)
        ]
    );
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 2);

    upstream.limited.store(false, Ordering::SeqCst);
    let blocked = live.bus_arrivals("NewTaipei", &stops(&["5"])).await;
    let cached = live.bus_arrivals("Taipei", &stops(&["2"])).await;

    assert_eq!(
        unavailable_stops(&blocked),
        [("5", UnavailableReason::RateLimited)]
    );
    assert_eq!(returned_stops(&cached), ["2"]);
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_rate_limit_without_retry_after_uses_the_configured_backoff() {
    let upstream = Upstream::default();
    let live = cached_live_data(upstream.clone()).await;
    upstream.limited.store(true, Ordering::SeqCst);
    live.bus_arrivals("Taipei", &stops(&["1"])).await;
    upstream.limited.store(false, Ordering::SeqCst);

    let blocked = live.bus_arrivals("Taipei", &stops(&["1"])).await;

    assert_eq!(
        unavailable_stops(&blocked),
        [("1", UnavailableReason::RateLimited)]
    );
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_zero_retry_after_does_not_block_the_next_request() {
    let upstream = Upstream {
        retry_after: Some("0"),
        ..Upstream::default()
    };
    let live = cached_live_data(upstream.clone()).await;
    upstream.limited.store(true, Ordering::SeqCst);
    live.bus_arrivals("Taipei", &stops(&["1"])).await;
    upstream.limited.store(false, Ordering::SeqCst);

    let recovered = live.bus_arrivals("Taipei", &stops(&["1"])).await;

    assert_eq!(returned_stops(&recovered), ["1"]);
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 2);
}
