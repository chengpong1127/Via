mod latest_link;
mod report;

use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use clap::{Args, Parser, Subcommand};
use latest_link::{update_latest_fetch_link, update_latest_static_link};
use report::{print_build_diagnostics, print_mode_coverage, print_static_counts};
use via_server::{
    config::{Environment, ViaConfig, load_dotenv, tdx_credentials},
    data::{
        build_from_fetch_snapshot, fetch,
        fetch::{FetchPolicy, FetchSources, fetch_snapshot},
        gtfs::measure_mode_coverage,
        load_static_artifact,
    },
    domain::{
        ids::BoardingPointId, transit::network::TransitMode, transit::runtime::TransitRuntime,
    },
    providers::valhalla::ValhallaProvider,
    street::{DependencyHealth, StreetRouter},
    tdx::TdxClient,
};

#[derive(Parser, Debug)]
#[command(
    name = "via-data",
    about = "Fetch, build, validate, and inspect Via static data"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Fetch(FetchArgs),
    Build(BuildArgs),
    Validate(StaticArgs),
    Inspect(InspectArgs),
    ValidateConfig,
}

#[derive(Args, Debug)]
struct FetchArgs {
    #[arg(long)]
    gtfs_file: Option<PathBuf>,
    #[arg(long)]
    gtfs_only: bool,
}

#[derive(Args, Debug)]
struct BuildArgs {
    #[arg(long, default_value_os_t = latest_fetch_snapshot())]
    input: PathBuf,
}

#[derive(Args, Debug)]
struct InspectArgs {
    #[command(subcommand)]
    command: InspectCommand,
}

#[derive(Args, Debug)]
struct StaticArgs {
    #[arg(long = "static", default_value_os_t = latest_static_artifact())]
    static_path: PathBuf,
}

fn latest_fetch_snapshot() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/fetch/latest")
}

fn latest_static_artifact() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/build/.via-static-latest.json")
}

fn require_existing(path: &Path, flag: &str, producer: &str) -> Result<(), Box<dyn Error>> {
    if path.exists() {
        return Ok(());
    }
    Err(format!(
        "{} does not exist; run `via-data {producer}` first or pass {flag}",
        path.display()
    )
    .into())
}

#[derive(Args, Debug)]
struct QueryArgs {
    #[command(flatten)]
    artifact: StaticArgs,
    #[arg(long)]
    query: String,
}

#[derive(Args, Debug)]
struct NearbyArgs {
    #[command(flatten)]
    artifact: StaticArgs,
    #[arg(long)]
    lat: f64,
    #[arg(long)]
    lon: f64,
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Subcommand, Debug)]
enum InspectCommand {
    Stop(QueryArgs),
    Pattern(QueryArgs),
    NearbyStops(NearbyArgs),
    Stats(StaticArgs),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    load_dotenv();
    let cli = Cli::parse();
    let settings = ViaConfig::load_for(
        cli.config.as_deref(),
        Environment::from_env_or_development()?,
    )?;
    match cli.command {
        Command::ValidateConfig => println!("configuration is valid"),
        Command::Fetch(args) => fetch(args, &settings).await?,
        Command::Build(args) => build(args, &settings).await?,
        Command::Validate(args) => validate(args)?,
        Command::Inspect(args) => inspect(args)?,
    }
    Ok(())
}

async fn fetch(args: FetchArgs, settings: &ViaConfig) -> Result<(), Box<dyn Error>> {
    let client = TdxClient::new(tdx_credentials()?, settings.tdx.clone())?;
    let api_base_url = url::Url::parse(&settings.tdx.api_base_url)?;
    let sources = FetchSources::production(
        api_base_url.as_str(),
        FetchPolicy::from_settings(&settings.tdx.fetch, args.gtfs_only),
    );
    let snapshot = fetch_snapshot(args.gtfs_file.as_deref(), &client, &sources).await?;
    let latest_snapshot = update_latest_fetch_link(&snapshot.path)?;
    println!("fetch snapshot: {}", snapshot.path.display());
    println!("latest fetch snapshot: {}", latest_snapshot.display());
    println!("fetched at: {}", snapshot.manifest.fetched_at);
    for (name, source) in &snapshot.manifest.sources {
        println!(
            "  {name}: source={} bytes={} sha256={}",
            source.source_id, source.byte_size, source.sha256
        );
    }
    if !snapshot.manifest.reused_sources.is_empty() {
        let reused = snapshot
            .manifest
            .reused_sources
            .iter()
            .map(|(name, fetched_at)| format!("{name} (from {fetched_at})"))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "reused {} source(s) from earlier snapshots: {reused}",
            snapshot.manifest.reused_sources.len()
        );
    }
    for (city, count) in snapshot.bike_station_counts {
        println!("  {city} Bike stations: {count}");
    }
    Ok(())
}

async fn build(args: BuildArgs, settings: &ViaConfig) -> Result<(), Box<dyn Error>> {
    require_existing(&args.input, "--input", "fetch")?;
    let started = Instant::now();
    let street = build_street_router(settings).await?;
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data/build")
        .join(fetch::timestamp_now());
    let result = build_from_fetch_snapshot(
        &args.input,
        &output,
        street.as_ref(),
        settings.preprocessing.bike_access,
        settings.preprocessing.transfers,
        settings.preprocessing.same_stop_transfer_seconds,
        settings.preprocessing.bus_derived_segment_scale_percent,
    )
    .await?;
    let latest_static = update_latest_static_link(&output)?;
    println!("build latency: {} ms", started.elapsed().as_millis());
    print_build_diagnostics(&result.diagnostics);
    print_static_counts("built", &result.via_static);
    println!(
        "static artifact: {}",
        output.join(".via-static.json").display()
    );
    println!("latest static artifact: {}", latest_static.display());
    Ok(())
}

async fn build_street_router(
    settings: &ViaConfig,
) -> Result<Arc<dyn StreetRouter>, Box<dyn Error>> {
    let valhalla_config = settings.valhalla.provider_config(true)?;
    let street = Arc::new(ValhallaProvider::new(valhalla_config)?);
    if !street.healthy().await {
        return Err("Valhalla is not healthy".into());
    }
    Ok(street)
}

fn load_static(path: &Path) -> Result<via_server::data::ViaStatic, Box<dyn Error>> {
    require_existing(path, "--static", "build")?;
    Ok(load_static_artifact(path)?)
}

fn load_transit(args: &StaticArgs) -> Result<TransitRuntime, Box<dyn Error>> {
    let artifact = load_static(&args.static_path)?;
    Ok(TransitRuntime::new(Arc::new(artifact.transit)))
}

fn validate(args: StaticArgs) -> Result<(), Box<dyn Error>> {
    let via_static = load_static(&args.static_path)?;
    print_static_counts("validated", &via_static);
    Ok(())
}

fn inspect(args: InspectArgs) -> Result<(), Box<dyn Error>> {
    match args.command {
        InspectCommand::Stop(args) => inspect_stop(args),
        InspectCommand::Pattern(args) => inspect_pattern(args),
        InspectCommand::NearbyStops(args) => inspect_nearby_stops(args),
        InspectCommand::Stats(args) => inspect_stats(args),
    }
}

fn inspect_stop(args: QueryArgs) -> Result<(), Box<dyn Error>> {
    let transit = load_transit(&args.artifact)?;
    let data = transit.network();
    let query = args.query.to_lowercase();
    let mut matches = data
        .stops
        .iter()
        .filter(|stop| stop.id.as_str() == args.query || stop.name.to_lowercase().contains(&query))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    if matches.is_empty() {
        println!("no stops matched {:?}", args.query);
        return Ok(());
    }
    for stop in matches {
        println!(
            "{}  {} ({:.6}, {:.6})",
            stop.id.as_str(),
            stop.name,
            stop.coordinate.latitude,
            stop.coordinate.longitude
        );
        for pattern in &data.patterns {
            if let Some(position) = pattern.stops.iter().position(|id| id == &stop.id) {
                println!(
                    "  pattern={} label={:?} position={}",
                    pattern.id.as_str(),
                    transit.required_route(&pattern.route_id).label,
                    position + 1
                );
            }
        }
    }
    Ok(())
}

fn inspect_pattern(args: QueryArgs) -> Result<(), Box<dyn Error>> {
    let transit = load_transit(&args.artifact)?;
    let data = transit.network();
    let query = args.query.to_lowercase();
    let mut matches = data
        .patterns
        .iter()
        .filter(|pattern| {
            pattern.id.as_str() == args.query
                || transit
                    .required_route(&pattern.route_id)
                    .label
                    .to_lowercase()
                    .contains(&query)
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    if matches.is_empty() {
        println!("no patterns matched {:?}", args.query);
        return Ok(());
    }
    for pattern in matches {
        println!(
            "pattern={} label={:?} mode={} headsign={:?}",
            pattern.id.as_str(),
            transit.required_route(&pattern.route_id).label,
            mode_name(transit.required_route(&pattern.route_id).mode),
            pattern.headsign
        );
        println!(
            "  stops: {}",
            pattern
                .stops
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        );
        println!(
            "  scheduled trips: {}",
            data.trips
                .iter()
                .filter(|trip| trip.pattern_id == pattern.id)
                .count()
        );
        println!(
            "  GTFS frequencies: {}",
            data.frequencies
                .iter()
                .filter(|frequency| frequency.pattern_id == pattern.id)
                .count()
        );
    }
    Ok(())
}

fn inspect_nearby_stops(args: NearbyArgs) -> Result<(), Box<dyn Error>> {
    let transit = load_transit(&args.artifact)?;
    let coordinate = coordinate(args.lat, args.lon)?;
    let mut stops = transit
        .network()
        .stops
        .iter()
        .map(|stop| (stop, coordinate.distance_metres(stop.coordinate)))
        .collect::<Vec<_>>();
    stops.sort_by(|(left, left_distance), (right, right_distance)| {
        left_distance
            .total_cmp(right_distance)
            .then(left.id.as_str().cmp(right.id.as_str()))
    });
    for (stop, distance) in stops.into_iter().take(args.limit.max(1)) {
        println!(
            "{:.0}m  {}  {}  {}",
            distance,
            stop.id.as_str(),
            stop.name,
            pattern_labels(&transit, &stop.id)
        );
    }
    Ok(())
}

fn inspect_stats(args: StaticArgs) -> Result<(), Box<dyn Error>> {
    let artifact = load_static(&args.static_path)?;
    print_static_counts("static data", &artifact);
    let coverage = measure_mode_coverage(&artifact.transit, &artifact.provider_bindings);
    print_mode_coverage(&coverage);
    let transit = TransitRuntime::new(Arc::new(artifact.transit));
    println!(
        "service-day boundary: {}",
        transit.network().service_day_boundary
    );
    Ok(())
}

fn coordinate(
    latitude: f64,
    longitude: f64,
) -> Result<via_server::api::GeoCoordinate, Box<dyn Error>> {
    let coordinate = via_server::api::GeoCoordinate {
        latitude,
        longitude,
    };
    if !coordinate.validate() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid coordinate {latitude},{longitude}"),
        )
        .into());
    }
    Ok(coordinate)
}

fn pattern_labels(transit: &TransitRuntime, stop_id: &BoardingPointId) -> String {
    transit
        .network()
        .patterns
        .iter()
        .filter(|pattern| pattern.stops.iter().any(|id| id == stop_id))
        .map(|pattern| transit.required_route(&pattern.route_id).label.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn mode_name(mode: TransitMode) -> &'static str {
    match mode {
        TransitMode::Bus => "Bus",
        TransitMode::Metro => "Metro",
        TransitMode::Rail => "TRA",
        TransitMode::HighSpeedRail => "THSR",
    }
}
