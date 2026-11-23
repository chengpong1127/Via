use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use via_server::{
    api::{DepartureSetting, PlanningMode, PlanningRequest, SelectedPlace},
    app::runtime::{ViaRuntime, ViaRuntimeDependencies},
    config::{Environment, ViaConfig, load_dotenv, tdx_credentials},
    data::load_static_artifact,
    domain::journey::{LegDetails, PlanningOutcome},
    live::{
        BikeAvailabilityRecord, BusArrival, CachedLiveData, DirectLiveData, LiveBatch,
        LiveDataSource, MetroBoard, MetroPair, TraDelay,
    },
    providers::{TdxBikeAvailabilitySource, TracingOperationalEventObserver, ValhallaProvider},
    tdx::TdxClient,
};

struct CountingLiveData {
    inner: Arc<dyn LiveDataSource>,
    requests: AtomicUsize,
}

impl CountingLiveData {
    fn record<T, K>(&self, batch: LiveBatch<T, K>) -> LiveBatch<T, K> {
        self.requests
            .fetch_add(batch.upstream_requests, Ordering::SeqCst);
        batch
    }
}

#[async_trait]
impl LiveDataSource for CountingLiveData {
    async fn bus_arrivals(&self, scope: &str, stop_uids: &[String]) -> LiveBatch<BusArrival> {
        self.record(self.inner.bus_arrivals(scope, stop_uids).await)
    }

    async fn bike_availability(
        &self,
        city: &str,
        station_uids: &[String],
    ) -> LiveBatch<BikeAvailabilityRecord> {
        self.record(self.inner.bike_availability(city, station_uids).await)
    }

    async fn metro_boards(&self, pairs: &[MetroPair]) -> LiveBatch<MetroBoard, MetroPair> {
        self.record(self.inner.metro_boards(pairs).await)
    }

    async fn tra_delays(&self, train_numbers: &[String]) -> LiveBatch<TraDelay> {
        self.record(self.inner.tra_delays(train_numbers).await)
    }
}

fn summarise(outcome: &PlanningOutcome) -> serde_json::Value {
    match outcome {
        PlanningOutcome::Empty(empty) => serde_json::json!({"empty": empty.empty.code}),
        PlanningOutcome::Options(options) => serde_json::json!(
            options
                .journey_options
                .iter()
                .map(|option| serde_json::json!({
                    "departure": option.departure_time.to_string(),
                    "arrival": option.arrival_time.unix_timestamp(),
                    "duration": option.duration_seconds,
                    "walking": option.walking_seconds,
                    "transfers": option.transfer_count,
                    "live": format!("{:?}", option.live_status_summary.state),
                    "legs": option.legs.iter().map(|leg| match &leg.details {
                        LegDetails::Walking => format!("walk {}s", leg.duration_seconds),
                        LegDetails::Transit { service_label, transit_mode, .. } => format!(
                            "{transit_mode:?} {service_label} {}>{} {}s",
                            leg.from.display_name, leg.to.display_name, leg.duration_seconds
                        ),
                        LegDetails::BikeShare { .. } => format!("bike {}s", leg.duration_seconds),
                    }).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        ),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();
    let args: Vec<String> = std::env::args().collect();
    let artifact = load_static_artifact(&PathBuf::from(&args[1]))?;
    let scenarios: Vec<(String, f64, f64, f64, f64)> =
        serde_json::from_str(&std::fs::read_to_string(&args[2])?)?;
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args[3])?;
    let environment = Environment::from_env_or_development()?;
    let settings = ViaConfig::load_for_with(None, environment, |settings| {
        settings.tdx.realtime.cache_ttl_seconds = 300;
    })?;
    let valhalla = Arc::new(ValhallaProvider::with_observer(
        settings.valhalla.provider_config(false)?,
        Arc::new(TracingOperationalEventObserver),
    )?);
    let client = Arc::new(TdxClient::new(tdx_credentials()?, settings.tdx.clone())?);
    let counter = Arc::new(CountingLiveData {
        inner: Arc::new(DirectLiveData::new(client)),
        requests: AtomicUsize::new(0),
    });
    let live: Arc<dyn LiveDataSource> =
        Arc::new(CachedLiveData::new(counter.clone(), &settings.tdx));
    let bike_share = Arc::new(artifact.bike_share);
    let names: Vec<String> = args
        .get(4)
        .map_or("base,place,est,place+est", String::as_str)
        .split(',')
        .map(str::to_owned)
        .collect();
    let runtime = ViaRuntime::from_dependencies(ViaRuntimeDependencies {
        transit_network: artifact.transit.clone(),
        provider_bindings: artifact.provider_bindings.clone(),
        bike_share_network: bike_share.clone(),
        street: valhalla.clone(),
        live: live.clone(),
        realtime: settings.tdx.realtime.clone(),
        bike_availability: Arc::new(TdxBikeAvailabilitySource::new(
            live.clone(),
            settings.tdx.bikeshare.clone(),
        )),
        limits: settings.journey_limits(environment),
    });
    let variants: Vec<_> = names.iter().map(|name| (name.as_str(), &runtime)).collect();
    for (index, (name, from_lat, from_lon, to_lat, to_lon)) in scenarios.into_iter().enumerate() {
        let before = counter.requests.load(Ordering::SeqCst);
        let departure = time::OffsetDateTime::now_utc();
        let mut record = serde_json::json!({"name": name, "planned_at": departure.to_string()});
        for offset in 0..variants.len() {
            let (variant, runtime) = &variants[(index + offset) % variants.len()];
            let request = PlanningRequest {
                origin: SelectedPlace {
                    coordinate: via_server::api::GeoCoordinate {
                        latitude: from_lat,
                        longitude: from_lon,
                    },
                    display_text: "origin".into(),
                },
                destination: SelectedPlace {
                    coordinate: via_server::api::GeoCoordinate {
                        latitude: to_lat,
                        longitude: to_lon,
                    },
                    display_text: "destination".into(),
                },
                selected_modes: vec![PlanningMode::Bus, PlanningMode::Metro, PlanningMode::Tra],
                departure_setting: DepartureSetting::DepartAt { time: departure },
            };
            via_server::domain::transit::topology::PLACE_EQUIVALENCE
                .store(variant.contains("place"), Ordering::SeqCst);
            via_server::providers::timing::HEADWAY_ESTIMATES
                .store(variant.contains("est"), Ordering::SeqCst);
            let variant_before = counter.requests.load(Ordering::SeqCst);
            let started = std::time::Instant::now();
            let outcome = runtime.routing.plan(request.into()).await;
            record[*variant] = match outcome {
                Ok(outcome) => summarise(&outcome),
                Err(error) => serde_json::json!({"error": format!("{error:?}")}),
            };
            record[format!("{variant}_ms")] = serde_json::json!(started.elapsed().as_millis());
            record[format!("{variant}_tdx")] =
                serde_json::json!(counter.requests.load(Ordering::SeqCst) - variant_before);
        }
        let used = counter.requests.load(Ordering::SeqCst) - before;
        record["tdx_requests"] = serde_json::json!(used);
        writeln!(out, "{record}")?;
        out.flush()?;
        eprintln!("{} tdx_requests={used}", record["name"]);
        tokio::time::sleep(Duration::from_secs(60 * used.max(1) as u64)).await;
    }
    Ok(())
}
