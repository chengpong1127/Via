use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use clap::{Args, Parser, Subcommand};
use via_server::{
    config::load_dotenv,
    data::{
        GtfsBuildDiagnostics, build_from_fetch_snapshot, fetch, fetch::fetch_snapshot,
        load_static_artifact,
    },
    domain::{ids::BoardingPointId, transit::TransitMode, transit_runtime::TransitRuntime},
    health::DependencyHealth,
    providers::valhalla::{ValhallaConfig, ValhallaProvider},
    street::StreetRouter,
};

#[derive(Parser, Debug)]
#[command(
    name = "via-data",
    about = "Fetch, build, validate, and inspect Via static data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Fetch(FetchArgs),
    Build(BuildArgs),
    Validate(ValidateArgs),
    Inspect(InspectArgs),
}

#[derive(Args, Debug)]
struct FetchArgs {
    #[arg(long)]
    gtfs_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct BuildArgs {
    #[arg(long)]
    input: PathBuf,
}

#[derive(Args, Debug)]
struct ValidateArgs {
    #[arg(long = "static")]
    static_path: PathBuf,
}

#[derive(Args, Debug)]
struct InspectArgs {
    #[command(subcommand)]
    command: InspectCommand,
}

#[derive(Args, Debug)]
struct StaticArgs {
    #[arg(long = "static")]
    static_path: PathBuf,
}

#[derive(Args, Debug)]
struct StopArgs {
    #[command(flatten)]
    artifact: StaticArgs,
    #[arg(long)]
    query: String,
}

#[derive(Args, Debug)]
struct PatternArgs {
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

#[derive(Args, Debug)]
struct StatsArgs {
    #[command(flatten)]
    artifact: StaticArgs,
}

#[derive(Subcommand, Debug)]
enum InspectCommand {
    Stop(StopArgs),
    Pattern(PatternArgs),
    NearbyStops(NearbyArgs),
    Stats(StatsArgs),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    load_dotenv();
    match Cli::parse().command {
        Command::Fetch(args) => fetch(args).await?,
        Command::Build(args) => build(args).await?,
        Command::Validate(args) => validate(args)?,
        Command::Inspect(args) => inspect(args)?,
    }
    Ok(())
}

async fn fetch(args: FetchArgs) -> Result<(), Box<dyn Error>> {
    let snapshot = fetch_snapshot(args.gtfs_file.as_deref()).await?;
    println!("fetch snapshot: {}", snapshot.path.display());
    println!("fetched at: {}", snapshot.manifest.fetched_at);
    for (name, source) in &snapshot.manifest.sources {
        println!(
            "  {name}: source={} bytes={} sha256={}",
            source.source_id, source.byte_size, source.sha256
        );
    }
    for (city, count) in snapshot.bike_station_counts {
        println!("  {city} Bike stations: {count}");
    }
    Ok(())
}

async fn build(args: BuildArgs) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let street = build_street_router().await?;
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data/build")
        .join(fetch::timestamp_now());
    let result = build_from_fetch_snapshot(&args.input, &output, street.as_ref()).await?;
    println!("build latency: {} ms", started.elapsed().as_millis());
    print_build_diagnostics(&result.diagnostics, &result.build.diagnostics);
    print_static_counts("built", &result.build.via_static);
    println!(
        "static artifact: {}",
        output.join(".via-static.json").display()
    );
    Ok(())
}

async fn build_street_router() -> Result<Arc<dyn StreetRouter>, Box<dyn Error>> {
    let mut config = ValhallaConfig::local();
    config.request_timeout = Duration::from_secs(15);
    if let Ok(base_url) = std::env::var("VIA_VALHALLA_URL") {
        config.base_url = url::Url::parse(&base_url)?;
    }
    let street = Arc::new(ValhallaProvider::new(config)?);
    if !street.healthy().await {
        return Err("Valhalla is not healthy".into());
    }
    Ok(street)
}

fn validate(args: ValidateArgs) -> Result<(), Box<dyn Error>> {
    let via_static = load_static_artifact(&args.static_path)?;
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

fn inspect_stop(args: StopArgs) -> Result<(), Box<dyn Error>> {
    let artifact = load_static_artifact(&args.artifact.static_path)?;
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    let data = runtime.data();
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
                    runtime.route_of(pattern).label,
                    position + 1
                );
            }
        }
    }
    Ok(())
}

fn inspect_pattern(args: PatternArgs) -> Result<(), Box<dyn Error>> {
    let artifact = load_static_artifact(&args.artifact.static_path)?;
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    let data = runtime.data();
    let query = args.query.to_lowercase();
    let mut matches = data
        .patterns
        .iter()
        .filter(|pattern| {
            pattern.id.as_str() == args.query
                || runtime
                    .route_of(pattern)
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
            runtime.route_of(pattern).label,
            mode_name(runtime.route_of(pattern).mode),
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
    let artifact = load_static_artifact(&args.artifact.static_path)?;
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    let coordinate = coordinate(args.lat, args.lon)?;
    for stop in runtime.nearest_stops(coordinate, args.limit.max(1)) {
        println!(
            "{:.0}m  {}  {}  {}",
            coordinate.haversine_distance_metres(stop.coordinate),
            stop.id.as_str(),
            stop.name,
            pattern_labels(&runtime, &stop.id)
        );
    }
    Ok(())
}

fn inspect_stats(args: StatsArgs) -> Result<(), Box<dyn Error>> {
    let artifact = load_static_artifact(&args.artifact.static_path)?;
    print_static_counts("static data", &artifact);
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    for mode in [
        TransitMode::Bus,
        TransitMode::Metro,
        TransitMode::Rail,
        TransitMode::HighSpeedRail,
    ] {
        println!(
            "{} pattern count: {}",
            mode_name(mode),
            runtime
                .data()
                .patterns
                .iter()
                .filter(|pattern| runtime.route_of(pattern).mode == mode)
                .count()
        );
    }
    println!(
        "derived Bus Phase 1 profiles: {}",
        runtime
            .data()
            .patterns
            .iter()
            .filter(|pattern| pattern
                .static_profile
                .segments
                .iter()
                .all(|segment| segment.source
                    == via_server::domain::transit::SegmentTimeSource::Derived))
            .count()
    );
    println!(
        "service-day boundary: {}",
        runtime.data().service_day_boundary
    );
    Ok(())
}

fn print_static_counts(prefix: &str, via_static: &via_server::data::ViaStatic) {
    println!("{prefix} ViaStatic");
    println!("  schema version: {}", via_static.schema_version);
    println!(
        "  transit revision: {}",
        via_static.revisions.transit_static
    );
    println!("  schedule revision: {}", via_static.revisions.schedule);
    println!("  BikeShare revision: {}", via_static.revisions.bike_share);
    println!(
        "  stops={} routes={} patterns={} trips={} calendars={} frequencies={} transfers={} bike_stations={} bike_access_links={}",
        via_static.counts.stops,
        via_static.counts.routes,
        via_static.counts.patterns,
        via_static.counts.trips,
        via_static.counts.calendars,
        via_static.counts.frequencies,
        via_static.counts.transfers,
        via_static.counts.bike_stations,
        via_static.counts.bike_access_links
    );
}

fn print_build_diagnostics(
    diagnostics: &GtfsBuildDiagnostics,
    build_diagnostics: &via_server::data::BuildDiagnostics,
) {
    if let Some(stats) = &build_diagnostics.static_transfers {
        println!(
            "walking transfers: boarding_points={} candidates={} matrix_pairs={} reachable={} retained={} removed_by_limits={} removed_by_fanout={} latency_ms={}",
            stats.boarding_points,
            stats.geographic_candidate_pairs,
            stats.matrix_pairs_queried,
            stats.reachable_walking_pairs,
            stats.retained_edges,
            stats.removed_by_limits,
            stats.removed_by_fanout,
            stats.elapsed_milliseconds
        );
    }
    if let Some(stats) = &build_diagnostics.bike_access {
        println!(
            "BikeShare access: boarding_points={} candidates={} walking_matrices={} access_links={} latency_ms={}",
            stats.boarding_points_processed,
            stats.geometric_candidate_pairs,
            stats.walking_matrix_requests,
            stats.retained_access_links,
            stats.build_wall_clock_milliseconds
        );
    }
    println!("GTFS stops loaded: {}", diagnostics.stops_loaded);
    println!("GTFS trips loaded: {}", diagnostics.trips_loaded);
    println!(
        "exact scheduled trips loaded: {}",
        diagnostics.scheduled_trips_loaded
    );
    println!(
        "GTFS frequencies loaded: {}",
        diagnostics.frequencies_loaded
    );
    println!("GTFS shape rows loaded: {}", diagnostics.shape_rows_loaded);
    println!(
        "GTFS trips skipped for invalid exact timing: {}",
        diagnostics.invalid_timing_trips
    );
    for mode in ["bus", "metro", "rail", "high_speed_rail"] {
        println!(
            "{mode}: routes={} patterns={}",
            diagnostics
                .routes_loaded_by_mode
                .get(mode)
                .copied()
                .unwrap_or(0),
            diagnostics
                .patterns_loaded_by_mode
                .get(mode)
                .copied()
                .unwrap_or(0),
        );
    }
    println!(
        "static Pattern profiles: exact={} derived_bus={} geometric_fallback_or_mixed={}",
        diagnostics.exact_static_timing_patterns,
        diagnostics.derived_bus_timing_patterns,
        diagnostics.geometric_fallback_patterns
    );
    println!(
        "Bus Patterns without a TDX realtime scope: {}",
        diagnostics.unresolved_bus_realtime_scope_patterns
    );
    println!(
        "GTFS stop IDs without current TDX ID mapping: {}",
        diagnostics.gtfs_stop_ids_without_tdx_mapping
    );
    println!(
        "Via Pattern IDs without current TDX realtime mapping: {}",
        diagnostics.gtfs_pattern_ids_without_tdx_mapping
    );
}

fn coordinate(
    latitude: f64,
    longitude: f64,
) -> Result<via_server::api::RequestCoordinate, Box<dyn Error>> {
    if !latitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !longitude.is_finite()
        || !(-180.0..=180.0).contains(&longitude)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid coordinate {latitude},{longitude}"),
        )
        .into());
    }
    Ok(via_server::api::RequestCoordinate {
        latitude,
        longitude,
    })
}

fn pattern_labels(network: &TransitRuntime, stop_id: &BoardingPointId) -> String {
    network
        .data()
        .patterns
        .iter()
        .filter(|pattern| pattern.stops.iter().any(|id| id == stop_id))
        .map(|pattern| network.route_of(pattern).label.as_str())
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
