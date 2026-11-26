use std::sync::{Arc, atomic::Ordering};

use async_trait::async_trait;
use metrics_exporter_prometheus::PrometheusBuilder;
use via_server::{
    config::ViaConfig,
    live::{
        BikeAvailabilityRecord, BusArrival, GatewayLiveData, LiveBatch, LiveDataSource, MetroBoard,
        MetroPair, TraDelay, Unavailable, UnavailableReason, gateway,
    },
};

use crate::cache::{Upstream, cached_live_data, stops};

async fn spawn_gateway(live: Arc<dyn LiveDataSource>, max_ids: usize) -> GatewayLiveData {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = gateway::router(
        live,
        PrometheusBuilder::new().build_recorder().handle(),
        max_ids,
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    GatewayLiveData::new(&format!("http://{address}"), &ViaConfig::default().tdx).unwrap()
}

struct Canned;

fn metro_pair(station_id: &str) -> MetroPair {
    MetroPair {
        line_id: "BL".into(),
        station_id: station_id.into(),
    }
}

#[async_trait]
impl LiveDataSource for Canned {
    async fn bus_arrivals(&self, scope: &str, stop_uids: &[String]) -> LiveBatch<BusArrival> {
        LiveBatch {
            records: vec![BusArrival {
                stop_uid: format!("{scope}:{}", stop_uids[0]),
                sub_route_uid: Some("service".into()),
                route_uid: None,
                direction: Some(1),
                stop_status: Some(0),
                estimate_seconds: Some(90),
                update_time: "2026-11-12T10:00:00+08:00".into(),
            }],
            unavailable: vec![Unavailable {
                id: stop_uids[1].clone(),
                reason: UnavailableReason::RateLimited,
            }],
            upstream_requests: 1,
            retry_after: None,
        }
    }

    async fn bike_availability(
        &self,
        city: &str,
        station_uids: &[String],
    ) -> LiveBatch<BikeAvailabilityRecord> {
        LiveBatch {
            records: vec![BikeAvailabilityRecord {
                station_uid: Some(format!("{city}:{}", station_uids[0])),
                service_status: Some(1),
                available_rent_bikes: Some(3),
                available_return_bikes: Some(4),
                source_update_time: None,
                update_time: Some("2026-11-12T10:00:00+08:00".into()),
            }],
            ..LiveBatch::default()
        }
    }

    async fn metro_boards(&self, pairs: &[MetroPair]) -> LiveBatch<MetroBoard, MetroPair> {
        LiveBatch {
            records: vec![MetroBoard {
                line_id: pairs[0].line_id.clone(),
                station_id: pairs[0].station_id.clone(),
                destination_station_id: "BL23".into(),
                service_status: 0,
                estimate_minutes: Some(2),
                source_update_time: "2026-11-12T10:00:00+08:00".into(),
                update_time: "2026-11-12T10:00:01+08:00".into(),
            }],
            unavailable: vec![Unavailable {
                id: pairs[1].clone(),
                reason: UnavailableReason::TimedOut,
            }],
            ..LiveBatch::default()
        }
    }

    async fn tra_delays(&self, train_numbers: &[String]) -> LiveBatch<TraDelay> {
        LiveBatch {
            records: vec![TraDelay {
                train_no: train_numbers[0].clone(),
                delay_minutes: 5,
                update_time: "2026-11-12T10:00:00+08:00".into(),
                board_update_time: "2026-11-12T10:00:01+08:00".into(),
                board_source_update_time: "2026-11-12T10:00:02+08:00".into(),
            }],
            ..LiveBatch::default()
        }
    }
}

#[tokio::test]
async fn every_dataset_round_trips_through_the_gateway() {
    let client = spawn_gateway(Arc::new(Canned), 10).await;
    let direct = Canned;

    let bus = client.bus_arrivals("Taipei", &stops(&["1", "2"])).await;
    let expected = direct.bus_arrivals("Taipei", &stops(&["1", "2"])).await;
    assert_eq!(bus.records, expected.records);
    assert_eq!(bus.unavailable, expected.unavailable);
    assert_eq!(bus.upstream_requests, 1);

    let bike = client.bike_availability("Taipei", &stops(&["A"])).await;
    assert_eq!(
        bike.records,
        direct
            .bike_availability("Taipei", &stops(&["A"]))
            .await
            .records
    );

    let pairs = [metro_pair("BL12"), metro_pair("BL13")];
    let metro = client.metro_boards(&pairs).await;
    let expected = direct.metro_boards(&pairs).await;
    assert_eq!(metro.records, expected.records);
    assert_eq!(metro.unavailable, expected.unavailable);

    let tra = client.tra_delays(&stops(&["123"])).await;
    assert_eq!(
        tra.records,
        direct.tra_delays(&stops(&["123"])).await.records
    );
}

#[tokio::test]
async fn replicas_behind_one_gateway_share_its_cache() {
    let upstream = Upstream::default();
    let live = cached_live_data(upstream.clone()).await;
    let first_replica = spawn_gateway(live.clone(), 10).await;
    let second_replica = first_replica.clone();

    let first = first_replica
        .bus_arrivals("Taipei", &stops(&["1", "2"]))
        .await;
    let second = second_replica
        .bus_arrivals("Taipei", &stops(&["2", "1"]))
        .await;

    assert_eq!(first.records.len(), 2);
    assert_eq!(second.records.len(), 2);
    assert_eq!(second.upstream_requests, 0);
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_unreachable_gateway_marks_every_id_unavailable() {
    let client = GatewayLiveData::new("http://127.0.0.1:1", &ViaConfig::default().tdx).unwrap();

    let batch = client.bus_arrivals("Taipei", &stops(&["1", "2"])).await;

    assert!(batch.records.is_empty());
    assert_eq!(
        batch
            .unavailable
            .iter()
            .map(|unavailable| (unavailable.id.as_str(), unavailable.reason))
            .collect::<Vec<_>>(),
        [
            ("1", UnavailableReason::UpstreamUnavailable),
            ("2", UnavailableReason::UpstreamUnavailable)
        ]
    );
}

#[tokio::test]
async fn the_gateway_rejects_more_ids_than_its_limit() {
    let client = spawn_gateway(Arc::new(Canned), 2).await;

    let batch = client
        .bus_arrivals("Taipei", &stops(&["1", "2", "3"]))
        .await;

    assert!(batch.records.is_empty());
    assert_eq!(batch.unavailable.len(), 3);
}

#[test]
fn a_gateway_url_replaces_the_tdx_credentials_requirement() {
    let mut settings = ViaConfig::default();
    settings.tdx.gateway.url = Some("http://via-tdx-gateway:3100".into());

    let config = via_server::config::RuntimeConfig::from_settings(
        via_server::config::Environment::Development,
        settings.clone(),
    );

    assert!(config.is_ok());
    assert!(via_server::app::runtime::live_data_source(None, &settings.tdx).is_ok());
    settings.tdx.gateway.url = None;
    assert!(via_server::app::runtime::live_data_source(None, &settings.tdx).is_err());
}
