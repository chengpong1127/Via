use std::{
    error::Error,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use clap::{Args, Parser, Subcommand};
use via_server::{
    config::{Environment, TdxRuntimeConfig, ViaConfig, load_dotenv},
    data::{
        GtfsModeCoverage, build_from_fetch_snapshot, fetch,
        fetch::{FetchSources, fetch_snapshot},
        gtfs::measure_mode_coverage,
        load_static_artifact,
    },
    domain::{ids::BoardingPointId, transit::TransitMode, transit_runtime::TransitRuntime},
    health::DependencyHealth,
    providers::valhalla::ValhallaProvider,
    street::StreetRouter,
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
    Validate(ValidateArgs),
    Inspect(InspectArgs),
    ValidateConfig,
}

#[derive(Args, Debug)]
struct FetchArgs {
    #[arg(long)]
    gtfs_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct BuildArgs {
    #[arg(long, default_value_os_t = latest_fetch_snapshot())]
    input: PathBuf,
}

#[derive(Args, Debug)]
struct ValidateArgs {
    #[arg(long = "static", default_value_os_t = latest_static_artifact())]
    static_path: PathBuf,
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
    let cli = Cli::parse();
    let mut settings = ViaConfig::load(cli.config.as_deref())?;
    settings.apply_environment()?;
    settings.validate_environment(Environment::from_env_or_development()?)?;
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
    let tdx = TdxRuntimeConfig::from_settings(&settings.tdx)?;
    let client = TdxClient::new(tdx.credentials, settings.tdx.clone())?;
    let sources = FetchSources::production(tdx.api_base_url.as_str());
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
        settings.bikeshare.preprocessing,
        settings.preprocessing.transfers,
        settings.preprocessing.same_stop_transfer_seconds,
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

fn update_latest_static_link(output: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let (output_directory, build_directory_name) = split_output_path(output, "build output")?;
    let latest_link = output_directory.join(".via-static-latest.json");
    let target = PathBuf::from(build_directory_name).join(".via-static.json");
    replace_latest_link(&latest_link, &target, create_file_symlink)?;
    Ok(latest_link)
}

fn update_latest_fetch_link(snapshot: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let (fetch_directory, snapshot_directory_name) = split_output_path(snapshot, "fetch snapshot")?;
    let latest_link = fetch_directory.join("latest");
    replace_latest_link(
        &latest_link,
        Path::new(snapshot_directory_name),
        create_directory_symlink,
    )?;
    Ok(latest_link)
}

fn split_output_path<'a>(
    path: &'a Path,
    description: &str,
) -> Result<(&'a Path, &'a std::ffi::OsStr), Box<dyn Error>> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{description} has no parent directory"),
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{description} has no directory name"),
        )
    })?;
    Ok((parent, name))
}

fn replace_latest_link(
    latest_link: &Path,
    target: &Path,
    create_symlink: fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(latest_link) {
        Ok(metadata) if metadata.file_type().is_symlink() => remove_symlink(latest_link)?,
        Ok(_) => {
            return Err(format!(
                "refusing to replace non-symlink latest path {}",
                latest_link.display()
            )
            .into());
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    create_symlink(target, latest_link)?;
    Ok(())
}

#[cfg(unix)]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    fs::remove_file(link)
}

#[cfg(windows)]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    fs::remove_file(link).or_else(|_| fs::remove_dir(link))
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
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

fn validate(args: ValidateArgs) -> Result<(), Box<dyn Error>> {
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

fn inspect_stop(args: StopArgs) -> Result<(), Box<dyn Error>> {
    let artifact = load_static(&args.artifact.static_path)?;
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    let data = runtime.network();
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
    let artifact = load_static(&args.artifact.static_path)?;
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    let data = runtime.network();
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
    let artifact = load_static(&args.artifact.static_path)?;
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    let coordinate = coordinate(args.lat, args.lon)?;
    let mut stops = runtime
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
            pattern_labels(&runtime, &stop.id)
        );
    }
    Ok(())
}

fn inspect_stats(args: StatsArgs) -> Result<(), Box<dyn Error>> {
    let artifact = load_static(&args.artifact.static_path)?;
    print_static_counts("static data", &artifact);
    let coverage = measure_mode_coverage(&artifact.transit, &artifact.provider_bindings);
    print_mode_coverage(&coverage);
    let runtime = TransitRuntime::new(Arc::new(artifact.transit));
    println!(
        "service-day boundary: {}",
        runtime.network().service_day_boundary
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

fn print_build_diagnostics(diagnostics: &via_server::data::BuildDiagnostics) {
    let gtfs = &diagnostics.gtfs;
    if let Some(stats) = &diagnostics.static_transfers {
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
    if let Some(stats) = &diagnostics.bike_access {
        println!(
            "BikeShare access: boarding_points={} candidates={} walking_matrices={} access_links={} latency_ms={}",
            stats.boarding_points_processed,
            stats.geometric_candidate_pairs,
            stats.walking_matrix_requests,
            stats.retained_access_links,
            stats.build_wall_clock_milliseconds
        );
    }
    println!("GTFS stops loaded: {}", gtfs.stops_loaded);
    println!("GTFS trips loaded: {}", gtfs.trips_loaded);
    println!(
        "exact scheduled trips loaded: {}",
        gtfs.scheduled_trips_loaded
    );
    println!("GTFS frequencies loaded: {}", gtfs.frequencies_loaded);
    println!("GTFS shape rows loaded: {}", gtfs.shape_rows_loaded);
    println!(
        "GTFS trips skipped for invalid exact timing: {}",
        gtfs.invalid_timing_trips
    );
    println!(
        "GTFS valid stops with no retained pattern: {}",
        gtfs.valid_unreferenced_stops
    );
    print_warning(
        "valid GTFS stop points are not referenced by any retained pattern",
        gtfs.valid_unreferenced_stops,
        &gtfs.unreferenced_stop_examples,
    );
    print_mode_coverage(&gtfs.coverage_by_mode);
    println!(
        "Bus Patterns without a TDX realtime scope: {}",
        gtfs.unresolved_bus_realtime_scope_patterns
    );
    println!(
        "GTFS stop IDs without current TDX ID mapping: {}",
        gtfs.gtfs_stop_ids_without_tdx_mapping
    );
    println!(
        "Via Pattern IDs without current TDX realtime mapping: {}",
        gtfs.gtfs_pattern_ids_without_tdx_mapping
    );
}

fn print_mode_coverage(coverage_by_mode: &std::collections::BTreeMap<String, GtfsModeCoverage>) {
    for line in mode_coverage_lines(coverage_by_mode) {
        println!("{line}");
    }
}

fn mode_coverage_lines(
    coverage_by_mode: &std::collections::BTreeMap<String, GtfsModeCoverage>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for mode in ["bus", "metro", "rail", "high_speed_rail"] {
        let coverage = coverage_by_mode.get(mode).cloned().unwrap_or_default();
        lines.push(format!(
            "{mode} coverage: routes={} routes_without_patterns={} patterns={} gtfs_service={} without_gtfs_service={} live_provider_bound={} without_usable_service={} exact_timing={} derived_bus_timing={} estimated_or_mixed_timing={}",
            coverage.supported_routes,
            coverage.routes_without_patterns,
            coverage.patterns,
            coverage.patterns_with_gtfs_service,
            coverage.patterns_without_gtfs_service,
            coverage.patterns_with_live_provider_binding,
            coverage.patterns_without_usable_service,
            coverage.exact_static_timing_patterns,
            coverage.derived_bus_timing_patterns,
            coverage.estimated_or_mixed_timing_patterns,
        ));
        lines.extend(formatted_warning_line(
            &format!("{mode} routes have no retained pattern"),
            coverage.routes_without_patterns,
            &coverage.route_without_pattern_examples,
        ));
        lines.extend(formatted_warning_line(
            &format!("{mode} patterns have no GTFS scheduled trips or frequencies"),
            coverage.patterns_without_gtfs_service,
            &coverage.patterns_without_gtfs_service_examples,
        ));
        lines.extend(formatted_warning_line(
            &format!("{mode} patterns have no usable static or realtime service"),
            coverage.patterns_without_usable_service,
            &coverage.patterns_without_usable_service_examples,
        ));
        lines.extend(formatted_warning_line(
            &format!("{mode} patterns use estimated or mixed Phase 1 timing"),
            coverage.estimated_or_mixed_timing_patterns,
            &coverage.estimated_or_mixed_timing_examples,
        ));
    }
    lines
}

fn formatted_warning_line(label: &str, count: usize, examples: &[String]) -> Option<String> {
    warning_line(label, count, examples).map(|line| format!("WARNING: {line}"))
}

fn print_warning(label: &str, count: usize, examples: &[String]) {
    if let Some(line) = warning_line(label, count, examples) {
        println!("WARNING: {line}");
    }
}

fn warning_line(label: &str, count: usize, examples: &[String]) -> Option<String> {
    (count > 0).then(|| {
        let examples = if examples.is_empty() {
            String::new()
        } else {
            format!("; examples: {}", examples.join(", "))
        };
        format!("{label}: {count}{examples}")
    })
}

fn coordinate(
    latitude: f64,
    longitude: f64,
) -> Result<via_server::api::GeoCoordinate, Box<dyn Error>> {
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
    Ok(via_server::api::GeoCoordinate {
        latitude,
        longitude,
    })
}

fn pattern_labels(network: &TransitRuntime, stop_id: &BoardingPointId) -> String {
    network
        .network()
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use super::{mode_coverage_lines, update_latest_fetch_link};
    use via_server::data::GtfsModeCoverage;

    #[test]
    fn latest_fetch_link_follows_the_newest_snapshot_and_preserves_real_directories() {
        let root = std::env::temp_dir().join(format!(
            "via-data-latest-fetch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let older = root.join("20260101T000000Z");
        let newer = root.join("20260201T000000Z");
        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&newer).unwrap();
        fs::write(older.join("manifest.json"), "older").unwrap();
        fs::write(newer.join("manifest.json"), "newer").unwrap();

        let latest = update_latest_fetch_link(&older).unwrap();
        assert_eq!(latest, root.join("latest"));
        assert_eq!(
            fs::read_to_string(latest.join("manifest.json")).unwrap(),
            "older"
        );
        update_latest_fetch_link(&newer).unwrap();
        assert_eq!(
            fs::read_link(&latest).unwrap(),
            std::path::PathBuf::from("20260201T000000Z")
        );
        assert_eq!(
            fs::read_to_string(latest.join("manifest.json")).unwrap(),
            "newer"
        );

        fs::remove_file(&latest).unwrap();
        fs::create_dir(&latest).unwrap();
        assert!(update_latest_fetch_link(&newer).is_err());
        assert!(fs::symlink_metadata(&latest).unwrap().is_dir());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn inspect_stats_mode_coverage_reports_counts_and_examples() {
        let coverage = GtfsModeCoverage {
            supported_routes: 3,
            routes_without_patterns: 1,
            route_without_pattern_examples: vec!["bus:missing".to_owned()],
            patterns: 2,
            patterns_with_gtfs_service: 1,
            patterns_without_gtfs_service: 1,
            patterns_without_gtfs_service_examples: vec!["bus:ghost".to_owned()],
            patterns_with_live_provider_binding: 1,
            patterns_without_usable_service: 1,
            patterns_without_usable_service_examples: vec!["bus:ghost".to_owned()],
            exact_static_timing_patterns: 1,
            derived_bus_timing_patterns: 0,
            estimated_or_mixed_timing_patterns: 1,
            estimated_or_mixed_timing_examples: vec!["bus:estimated".to_owned()],
        };
        let lines = mode_coverage_lines(&BTreeMap::from([("bus".to_owned(), coverage)]));

        assert!(lines.iter().any(|line| {
            line == "bus coverage: routes=3 routes_without_patterns=1 patterns=2 gtfs_service=1 without_gtfs_service=1 live_provider_bound=1 without_usable_service=1 exact_timing=1 derived_bus_timing=0 estimated_or_mixed_timing=1"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus routes have no retained pattern: 1; examples: bus:missing"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus patterns have no GTFS scheduled trips or frequencies: 1; examples: bus:ghost"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus patterns have no usable static or realtime service: 1; examples: bus:ghost"
        }));
        assert!(lines.iter().any(|line| {
            line == "WARNING: bus patterns use estimated or mixed Phase 1 timing: 1; examples: bus:estimated"
        }));
    }
}
