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
    api::{DepartureSetting, PlanningRequest, SelectedPlace},
    config::{TdxClient, TdxCredentials, load_dotenv},
    data::load_network_artifact,
    domain::{
        ids::TransitServiceId,
        journey::{JourneyMode, PlanningOutcome},
        realtime::{FreshnessState, NormalizedRealtimeValue},
    },
    health::DependencyHealth,
    providers::realtime::{ArrivalQuery, CachedRealtimeProvider, RealtimeError, RealtimeProvider},
    providers::{
        BusDepartureSource, BusRealtimeResolver, DepartureDispatcher, MetroDepartureSource,
        TdxRealtimeConfig, TdxRealtimeProvider, ThsrDepartureSource, TraDepartureSource,
        TransitDepartureSource, ValhallaConfig, ValhallaProvider,
    },
    routing::{RoutingLimits, RoutingService},
    street::{StraightLineStreetRouter, StreetRouter},
};

#[derive(Parser, Debug)]
#[command(
    name = "via-route",
    about = "Run a query through Via's production routing stack"
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
    #[arg(long, default_value = "transit")]
    modes: String,
    #[arg(long)]
    max_departures_per_query: Option<usize>,
    #[arg(long, value_enum, default_value_t = RealtimeMode::Fixture)]
    realtime: RealtimeMode,
    #[arg(long)]
    trace: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RealtimeMode {
    Fixture,
    Live,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    load_dotenv();
    let cli = Cli::parse();
    if cli.trace {
        tracing_subscriber::fmt()
            .with_env_filter("via::routing=debug")
            .without_time()
            .init();
    }
    let network = Arc::new(load_network_artifact(&cli.network)?);

    let realtime: Arc<dyn RealtimeProvider> = match cli.realtime {
        RealtimeMode::Fixture => Arc::new(FixtureRealtimeProvider),
        RealtimeMode::Live => build_live_realtime().await?,
    };
    let trace_events = Arc::new(Mutex::new(Vec::new()));
    let realtime: Arc<dyn RealtimeProvider> = if cli.trace {
        Arc::new(TraceRealtimeProvider {
            inner: realtime,
            events: trace_events.clone(),
        })
    } else {
        realtime
    };
    let cached = Arc::new(CachedRealtimeProvider::new(realtime));
    let realtime_metrics = cached.metrics();
    let street = build_street_router(cli.realtime).await?;
    let bus = Arc::new(BusDepartureSource::new(
        network.clone(),
        Some(Arc::new(
            BusRealtimeResolver::new(cached.clone(), network.clone())
                .with_metrics(realtime_metrics.clone()),
        )),
    ));
    let metro = Arc::new(MetroDepartureSource::new(network.clone()));
    let tra = Arc::new(TraDepartureSource::new(network.clone()));
    let thsr = Arc::new(ThsrDepartureSource::new(network.clone()));
    let departure = Arc::new(DepartureDispatcher::new(
        network.clone(),
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
    let routing = RoutingService::new(street, departure, cached.clone())
        .with_transit_network_arc(network.clone())
        .with_limits(limits);
    let request = request_from_cli(&cli)?;
    if cli.trace {
        eprintln!("ACCESS");
        eprintln!("  origin ({:.6}, {:.6})", cli.from_lat, cli.from_lon);
        eprintln!("  destination ({:.6}, {:.6})", cli.to_lat, cli.to_lon);
        eprintln!("  modes={}", cli.modes);
        eprintln!("  realtime={:?}", cli.realtime);
        eprintln!("  network={}", network.static_network_revision);
    }
    let result = routing
        .route(request)
        .await
        .map_err(|error| format!("routing failed: {error:?}"))?;
    if cli.trace {
        eprintln!("{}", realtime_metrics.bus_timing_trace());
        print_trace(&result, &trace_events);
    }
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_summary(
            &result,
            &network.static_network_revision,
            &network.schedule_revision,
            cli.realtime,
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

async fn build_live_realtime() -> Result<Arc<dyn RealtimeProvider>, Box<dyn Error>> {
    let base_url = env::var("VIA_TDX_BASE_URL")
        .unwrap_or_else(|_| "https://tdx.transportdata.tw/api/basic/".into());
    let base_url = url::Url::parse(&base_url)?;
    let tdx_client = TdxClient::with_api_base_url(TdxCredentials::from_env()?, base_url.as_str())?;
    let config = TdxRealtimeConfig {
        base_url: base_url.clone(),
    };
    let mut provider = TdxRealtimeProvider::new(config, tdx_client)?;
    if let Ok(path) = env::var("VIA_TDX_SERVICE_CONDITION_PATH") {
        provider = provider.with_service_condition_path(path);
    }
    Ok(Arc::new(provider))
}

async fn build_street_router(
    realtime: RealtimeMode,
) -> Result<Arc<dyn StreetRouter>, Box<dyn Error>> {
    if matches!(realtime, RealtimeMode::Fixture) {
        return Ok(Arc::new(StraightLineStreetRouter::default()));
    }
    let graph_revision = env::var("VIA_VALHALLA_GRAPH_REVISION")?;
    let tileset_last_modified = env::var("VIA_VALHALLA_TILESET_LAST_MODIFIED")?.parse()?;
    let mut config = ValhallaConfig::local(graph_revision, tileset_last_modified);
    if let Ok(base_url) = env::var("VIA_VALHALLA_URL") {
        config.base_url = url::Url::parse(&base_url)?;
    }
    let provider = ValhallaProvider::new(config)?;
    if !provider.healthy().await {
        return Err("Valhalla is not healthy for the configured graph artifact".into());
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

fn parse_modes(value: &str) -> Result<Vec<JourneyMode>, Box<dyn Error>> {
    let mut modes = value
        .split(',')
        .map(str::trim)
        .map(|mode| match mode {
            "transit" => Ok(JourneyMode::Transit),
            _ => Err(format!("unsupported mode {mode:?}; use transit")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    modes.sort();
    modes.dedup();
    if modes.is_empty() {
        return Err("at least one routing mode is required".into());
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
    realtime: RealtimeMode,
) {
    println!("network revision: {network_revision}");
    println!("schedule revision: {schedule_revision}");
    println!(
        "realtime mode: {}",
        match realtime {
            RealtimeMode::Fixture => "fixture",
            RealtimeMode::Live => "live",
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
    }
}

fn print_trace(result: &PlanningOutcome, events: &Arc<Mutex<Vec<String>>>) {
    eprintln!("REALTIME");
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
                            via_service_id,
                            via_boarding_point_id,
                            via_alighting_point_id,
                            service_label,
                            ..
                        } => eprintln!(
                            "  TRANSIT service={} label={:?} board={} alight={} ({}s)",
                            via_service_id,
                            service_label,
                            via_boarding_point_id,
                            via_alighting_point_id,
                            leg.duration_seconds
                        ),
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct FixtureRealtimeProvider;

struct TraceRealtimeProvider {
    inner: Arc<dyn RealtimeProvider>,
    events: Arc<Mutex<Vec<String>>>,
}

impl TraceRealtimeProvider {
    fn record(&self, event: String) {
        self.events
            .lock()
            .expect("trace event lock is not poisoned")
            .push(event);
    }
}

#[async_trait]
impl RealtimeProvider for TraceRealtimeProvider {
    async fn arrival_estimates(
        &self,
        query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.record(format!(
            "DEPARTURE services={} stops={}",
            join_ids(
                query
                    .transit_service_ids
                    .iter()
                    .map(TransitServiceId::as_str)
            ),
            join_ids(query.boarding_point_ids.iter().map(|id| id.as_str()))
        ));
        let result = self.inner.arrival_estimates(query).await;
        self.record(format_result(&result));
        result
    }

    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.record(format!(
            "CONDITION services={}",
            join_ids(service_ids.iter().map(TransitServiceId::as_str))
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

fn format_result(result: &Result<Vec<NormalizedRealtimeValue>, RealtimeError>) -> String {
    match result {
        Ok(values) => format!("provider result values={}", values.len()),
        Err(error) => format!("provider result error={error}"),
    }
}

#[async_trait]
impl RealtimeProvider for FixtureRealtimeProvider {
    async fn arrival_estimates(
        &self,
        _query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        Ok(Vec::new())
    }

    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        Ok(Vec::new())
    }
}
