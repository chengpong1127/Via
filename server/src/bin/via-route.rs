use std::{
    env,
    error::Error,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use clap::{Parser, ValueEnum};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    api::{DepartureSetting, PlanningRequest, SelectedPlace, TransportMode, new_request_id},
    config::load_dotenv,
    data::{load_network_artifact, load_provider_bindings_artifact},
    domain::{
        bikeshare::BikeShareNetwork,
        ids::TransitPatternId,
        journey::PlanningOutcome,
        realtime::{FreshnessState, NormalizedRealtimeValue},
        transit_runtime::TransitRuntime,
    },
    health::DependencyHealth,
    providers::service_conditions::{
        CachedServiceConditionProvider, ServiceConditionError, ServiceConditionProvider,
    },
    providers::timing::BusTimingMetrics,
    providers::{
        BusDepartureSource, DepartureDispatcher, MetroDepartureSource, TdxServiceConditionProvider,
        ThsrDepartureSource, TraDepartureSource, TransitDepartureSource,
        UnavailableBikeAvailabilitySource, ValhallaConfig, ValhallaProvider,
    },
    routing::{RoutingLimits, RoutingService},
    street::{StraightLineStreetRouter, StreetRouter},
    tdx::{TdxClient, TdxCredentials},
};

#[derive(Parser, Debug)]
#[command(
    name = "via-route",
    about = "Run an offline transit query through Via's production routing stack"
)]
struct Cli {
    #[arg(long)]
    network: PathBuf,
    #[arg(long)]
    from_lat: f64,
    #[arg(long)]
    from_lon: f64,
    #[arg(long)]
    to_lat: f64,
    #[arg(long)]
    to_lon: f64,
    #[arg(long, conflicts_with_all = ["depart_at", "arrive_by"])]
    depart_now: bool,
    #[arg(long, conflicts_with = "arrive_by")]
    depart_at: Option<String>,
    #[arg(long, conflicts_with = "depart_at")]
    arrive_by: Option<String>,
    #[arg(long, default_value = "bus,metro,tra,thsr")]
    modes: String,
    #[arg(long)]
    max_departures_per_query: Option<usize>,
    #[arg(long)]
    valhalla_timeout_ms: Option<u64>,
    #[arg(long = "service-conditions", value_enum, default_value_t = ServiceConditionMode::Fixture)]
    service_conditions: ServiceConditionMode,
    #[arg(long)]
    trace: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ServiceConditionMode {
    Fixture,
    Live,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    load_dotenv();
    let cli = Cli::parse();
    if cli.trace {
        tracing_subscriber::fmt()
            .with_env_filter("via_server::routing=debug")
            .without_time()
            .init();
    }
    let network = Arc::new(load_network_artifact(&cli.network)?);
    let provider_bindings = Arc::new(load_provider_bindings_artifact(
        &cli.network.with_file_name("provider-bindings.json"),
    )?);
    provider_bindings.validate(&network)?;
    let runtime = Arc::new(TransitRuntime::new(network.clone()));

    let (service_conditions, tdx): (Arc<dyn ServiceConditionProvider>, Option<Arc<TdxClient>>) =
        match cli.service_conditions {
            ServiceConditionMode::Fixture => (Arc::new(FixtureServiceConditionProvider), None),
            ServiceConditionMode::Live => {
                let (provider, tdx) =
                    build_live_service_conditions(provider_bindings.clone()).await?;
                (provider, Some(tdx))
            }
        };
    let trace_events = Arc::new(Mutex::new(Vec::new()));
    let service_conditions: Arc<dyn ServiceConditionProvider> = if cli.trace {
        Arc::new(TraceServiceConditionProvider {
            inner: service_conditions,
            events: trace_events.clone(),
        })
    } else {
        service_conditions
    };
    let cached = Arc::new(CachedServiceConditionProvider::new(service_conditions));
    let bus_timing_metrics = BusTimingMetrics::default();
    let street = build_street_router(cli.service_conditions, cli.valhalla_timeout_ms).await?;
    let bus = Arc::new(
        BusDepartureSource::new(runtime.clone(), tdx.clone())
            .with_bindings(provider_bindings.clone())
            .with_metrics(bus_timing_metrics.clone()),
    );
    let metro = Arc::new(
        MetroDepartureSource::new(runtime.clone())
            .with_bindings(provider_bindings.clone())
            .with_tdx(tdx.clone()),
    );
    let tra = Arc::new(
        TraDepartureSource::new(runtime.clone())
            .with_bindings(provider_bindings)
            .with_tdx(tdx),
    );
    let thsr = Arc::new(ThsrDepartureSource::new(runtime.clone()));
    let departure = Arc::new(DepartureDispatcher::new(
        runtime.clone(),
        vec![
            bus as Arc<dyn TransitDepartureSource>,
            metro as Arc<dyn TransitDepartureSource>,
            tra as Arc<dyn TransitDepartureSource>,
            thsr as Arc<dyn TransitDepartureSource>,
        ],
    ));
    let mut limits = RoutingLimits::default();
    if let Some(max_departures) = cli.max_departures_per_query {
        limits.max_departures_per_query = max_departures;
    }
    if let Some(timeout_ms) = cli.valhalla_timeout_ms {
        limits.request_timeout = std::time::Duration::from_millis(timeout_ms.saturating_mul(5));
    }
    let request = request_from_cli(&cli)?;
    if request.selected_modes.contains(&TransportMode::BikeShare) {
        return Err(
            "via-route is an offline transit-only debugging tool and does not support bike_share; use the production server for BikeShare routing".into(),
        );
    }
    let bike_share_network =
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("cli-disabled"));
    let bike_availability_source = Arc::new(UnavailableBikeAvailabilitySource);
    let routing = RoutingService::new(
        street,
        runtime.clone(),
        departure,
        cached,
        bike_share_network,
        bike_availability_source,
        limits,
    );
    if cli.trace {
        eprintln!("ACCESS");
        eprintln!("  origin ({:.6}, {:.6})", cli.from_lat, cli.from_lon);
        eprintln!("  destination ({:.6}, {:.6})", cli.to_lat, cli.to_lon);
        eprintln!("  modes={}", cli.modes);
        eprintln!("  service conditions={:?}", cli.service_conditions);
        eprintln!("  network={}", runtime.data().static_network_revision);
    }
    let result = routing
        .route(request, new_request_id())
        .await
        .map_err(|error| format!("routing failed: {error:?}"))?;
    if cli.trace {
        eprintln!("{}", bus_timing_metrics.trace());
        print_trace(&result, &trace_events);
    }
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_summary(
            &result,
            &runtime.data().static_network_revision,
            &runtime.data().schedule_revision,
            cli.service_conditions,
        );
    }
    if cli.trace {
        eprintln!("RESULT");
        match &result {
            PlanningOutcome::Options(options) => {
                eprintln!("  options={}", options.journey_options.len())
            }
            PlanningOutcome::Empty(_) => eprintln!("  options=0"),
        }
    }
    Ok(())
}

async fn build_live_service_conditions(
    bindings: Arc<via_server::providers::bindings::TdxProviderBindings>,
) -> Result<(Arc<dyn ServiceConditionProvider>, Arc<TdxClient>), Box<dyn Error>> {
    let base_url = env::var("VIA_TDX_BASE_URL")
        .unwrap_or_else(|_| "https://tdx.transportdata.tw/api/basic/".into());
    let base_url = url::Url::parse(&base_url)?;
    let tdx_client = Arc::new(TdxClient::with_api_base_url(
        TdxCredentials::from_env()?,
        base_url.as_str(),
    )?);
    let mut provider = TdxServiceConditionProvider::new(tdx_client.clone()).with_bindings(bindings);
    if let Ok(path) = env::var("VIA_TDX_SERVICE_CONDITION_PATH") {
        provider = provider.with_service_condition_path(path);
    }
    Ok((Arc::new(provider), tdx_client))
}

async fn build_street_router(
    service_conditions: ServiceConditionMode,
    timeout_ms: Option<u64>,
) -> Result<Arc<dyn StreetRouter>, Box<dyn Error>> {
    if matches!(service_conditions, ServiceConditionMode::Fixture) {
        return Ok(Arc::new(StraightLineStreetRouter::default()));
    }
    let mut config = ValhallaConfig::local();
    if let Some(timeout_ms) = timeout_ms {
        config.request_timeout = std::time::Duration::from_millis(timeout_ms);
    }
    if let Ok(base_url) = env::var("VIA_VALHALLA_URL") {
        config.base_url = url::Url::parse(&base_url)?;
    }
    let provider = ValhallaProvider::new(config)?;
    if !provider.healthy().await {
        return Err("Valhalla is not healthy".into());
    }
    Ok(Arc::new(provider))
}

fn request_from_cli(cli: &Cli) -> Result<PlanningRequest, Box<dyn Error>> {
    let selected_modes = parse_modes(&cli.modes)?;
    let departure_setting = if let Some(value) = &cli.depart_at {
        DepartureSetting::DepartAt {
            time: parse_time(value)?,
        }
    } else if let Some(value) = &cli.arrive_by {
        DepartureSetting::ArriveBy {
            time: parse_time(value)?,
        }
    } else {
        let _ = cli.depart_now;
        DepartureSetting::DepartNow
    };
    let request = PlanningRequest {
        origin: SelectedPlace {
            coordinate: via_server::api::RequestCoordinate {
                latitude: cli.from_lat,
                longitude: cli.from_lon,
            },
            display_text: "origin".into(),
        },
        destination: SelectedPlace {
            coordinate: via_server::api::RequestCoordinate {
                latitude: cli.to_lat,
                longitude: cli.to_lon,
            },
            display_text: "destination".into(),
        },
        selected_modes,
        locale: "en-US".into(),
        departure_setting,
    };
    request
        .validate()
        .map_err(|error| Box::new(error) as Box<dyn Error>)?;
    Ok(request)
}

fn parse_modes(value: &str) -> Result<Vec<TransportMode>, Box<dyn Error>> {
    let mut modes = value
        .split(',')
        .map(str::trim)
        .map(|mode| match mode {
            "bus" => Ok(TransportMode::Bus),
            "metro" => Ok(TransportMode::Metro),
            "tra" => Ok(TransportMode::Tra),
            "thsr" => Ok(TransportMode::Thsr),
            "bike_share" => Ok(TransportMode::BikeShare),
            _ => Err(format!(
                "unsupported mode {mode:?}; use bus, metro, tra, thsr, or bike_share"
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    modes.sort();
    if modes.is_empty() {
        return Err("at least one routing mode is required".into());
    }
    if modes.windows(2).any(|modes| modes[0] == modes[1]) {
        return Err("routing modes must not contain duplicates".into());
    }
    Ok(modes)
}

fn parse_time(value: &str) -> Result<OffsetDateTime, Box<dyn Error>> {
    Ok(OffsetDateTime::parse(value, &Rfc3339)?)
}

fn print_summary(
    result: &PlanningOutcome,
    network_revision: &str,
    schedule_revision: &str,
    service_conditions: ServiceConditionMode,
) {
    println!("network revision: {network_revision}");
    println!("schedule revision: {schedule_revision}");
    println!(
        "service-condition mode: {}",
        match service_conditions {
            ServiceConditionMode::Fixture => "fixture",
            ServiceConditionMode::Live => "live",
        }
    );
    match result {
        PlanningOutcome::Empty(empty) => {
            println!("no feasible journey option ({})", empty.empty.code)
        }
        PlanningOutcome::Options(options) => {
            for (index, option) in options.journey_options.iter().enumerate() {
                println!(
                    "\n{}. {} min",
                    index + 1,
                    option.duration_seconds.div_ceil(60)
                );
                println!("   {} → {}", option.departure_time, option.arrival_time);
                println!(
                    "   timing basis: {}",
                    if option.live_status_summary.state == FreshnessState::Fresh {
                        "realtime evidence"
                    } else {
                        "schedule/fallback"
                    }
                );
                for leg in &option.legs {
                    println!("   {}", leg_summary(leg));
                }
            }
        }
    }
}

fn leg_summary(leg: &via_server::domain::journey::Leg) -> String {
    match &leg.details {
        via_server::domain::journey::LegDetails::Walking { .. } => {
            format!("Walk {} min", leg.duration_seconds.div_ceil(60))
        }
        via_server::domain::journey::LegDetails::Transit {
            service_label,
            transit_mode,
            ..
        } => format!(
            "{transit_mode:?}: {service_label} ({} min)",
            leg.duration_seconds.div_ceil(60)
        ),
        via_server::domain::journey::LegDetails::BikeShare { .. } => {
            format!("Bike share ({} min)", leg.duration_seconds.div_ceil(60))
        }
    }
}

fn print_trace(result: &PlanningOutcome, events: &Arc<Mutex<Vec<String>>>) {
    eprintln!("SERVICE CONDITIONS");
    let events = events.lock().expect("trace event lock is not poisoned");
    if events.is_empty() {
        eprintln!("  no provider calls");
    } else {
        for event in events.iter() {
            eprintln!("  {event}");
        }
    }
    match result {
        PlanningOutcome::Empty(_) => eprintln!("ROUTE\n  no feasible journey"),
        PlanningOutcome::Options(options) => {
            for (index, option) in options.journey_options.iter().enumerate() {
                eprintln!(
                    "ROUTE {}\n  depart={} arrive={} transfers={} walking={}s",
                    index + 1,
                    option.departure_time,
                    option.arrival_time,
                    option.transfer_count,
                    option.walking_seconds
                );
                for leg in &option.legs {
                    match &leg.details {
                        via_server::domain::journey::LegDetails::Walking { .. } => eprintln!(
                            "  STREET walk {} -> {} ({}s)",
                            leg.from.display_name, leg.to.display_name, leg.duration_seconds
                        ),
                        via_server::domain::journey::LegDetails::Transit {
                            via_pattern_id,
                            via_boarding_point_id,
                            via_alighting_point_id,
                            service_label,
                            ..
                        } => eprintln!(
                            "  TRANSIT service={} label={:?} board={} alight={} ({}s)",
                            via_pattern_id,
                            service_label,
                            via_boarding_point_id,
                            via_alighting_point_id,
                            leg.duration_seconds
                        ),
                        via_server::domain::journey::LegDetails::BikeShare {
                            pickup_station_id,
                            dropoff_station_id,
                            ..
                        } => eprintln!(
                            "  BICYCLE pickup={} dropoff={} ({}s)",
                            pickup_station_id, dropoff_station_id, leg.duration_seconds
                        ),
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct FixtureServiceConditionProvider;

struct TraceServiceConditionProvider {
    inner: Arc<dyn ServiceConditionProvider>,
    events: Arc<Mutex<Vec<String>>>,
}

impl TraceServiceConditionProvider {
    fn record(&self, event: String) {
        self.events
            .lock()
            .expect("trace event lock is not poisoned")
            .push(event);
    }
}

#[async_trait]
impl ServiceConditionProvider for TraceServiceConditionProvider {
    async fn service_conditions(
        &self,
        service_ids: &[TransitPatternId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError> {
        self.record(format!(
            "SERVICE CONDITIONS services={}",
            join_ids(service_ids.iter().map(TransitPatternId::as_str))
        ));
        let result = self.inner.service_conditions(service_ids).await;
        self.record(format_result(&result));
        result
    }
}

fn join_ids<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    let value = ids.collect::<Vec<_>>().join(",");
    if value.is_empty() { "-".into() } else { value }
}

fn format_result(result: &Result<Vec<NormalizedRealtimeValue>, ServiceConditionError>) -> String {
    match result {
        Ok(values) => format!("service-condition result values={}", values.len()),
        Err(error) => format!("service-condition result error={error}"),
    }
}

#[async_trait]
impl ServiceConditionProvider for FixtureServiceConditionProvider {
    async fn service_conditions(
        &self,
        _service_ids: &[TransitPatternId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError> {
        Ok(Vec::new())
    }
}
