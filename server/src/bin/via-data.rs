use std::{
    env, error::Error, io::IsTerminal, path::PathBuf, str::FromStr, sync::Arc, time::Duration,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use time::Date;
use via_server::{
    config::{TdxCredentials, load_dotenv},
    data::{
        ArtifactCounts, BuildDiagnostics, DataError, DataScope, FetchProgress, build_from_raw,
        build_from_raw_with_transfer_preprocessing_with_progress, fetch_raw_snapshot_with_progress,
        load_manifest, load_network_artifact, load_transfer_artifact,
    },
    domain::{
        ids::BoardingPointId,
        transit::{TransitMode, TransitNetwork},
    },
    providers::{ValhallaConfig, ValhallaProvider},
    routing::{RoutingLimits, TransferPreprocessingProgress},
    street::{StraightLineStreetRouter, StreetRouter},
};

#[derive(Parser, Debug)]
#[command(
    name = "via-data",
    about = "Fetch, build, validate, and inspect Via data artifacts"
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
    #[arg(long, default_value = "taipei-new-taipei")]
    scope: String,
    #[arg(long)]
    date: String,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Args, Debug)]
struct BuildArgs {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    scope: Option<String>,
    #[arg(long)]
    date: Option<String>,
    /// Explicitly run the bounded street-backed transfer preprocessor.
    #[arg(long)]
    preprocess_transfers: bool,
    #[arg(long, value_enum)]
    street_router: Option<BuildStreetRouter>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BuildStreetRouter {
    Valhalla,
    StraightLine,
}

#[derive(Args, Debug)]
struct ValidateArgs {
    #[arg(long)]
    artifact: PathBuf,
    #[arg(long)]
    transfer_artifact: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct InspectArgs {
    #[command(subcommand)]
    command: InspectCommand,
}

#[derive(Subcommand, Debug)]
enum InspectCommand {
    Stop(StopArgs),
    Service(ServiceArgs),
    NearbyStops(NearbyArgs),
    BikeNear(NearbyArgs),
    Stats(StatsArgs),
}

#[derive(Args, Debug)]
struct ArtifactArgs {
    #[arg(long)]
    artifact: PathBuf,
}

#[derive(Args, Debug)]
struct StopArgs {
    #[command(flatten)]
    artifact: ArtifactArgs,
    #[arg(long)]
    query: String,
}

#[derive(Args, Debug)]
struct ServiceArgs {
    #[command(flatten)]
    artifact: ArtifactArgs,
    #[arg(long)]
    query: String,
}

#[derive(Args, Debug)]
struct NearbyArgs {
    #[command(flatten)]
    artifact: ArtifactArgs,
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
    artifact: ArtifactArgs,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    load_dotenv();
    let cli = Cli::parse();
    match cli.command {
        Command::Fetch(args) => fetch(args).await?,
        Command::Build(args) => build(args).await?,
        Command::Validate(args) => validate(args)?,
        Command::Inspect(args) => inspect(args)?,
    }
    Ok(())
}

async fn fetch(args: FetchArgs) -> Result<(), Box<dyn Error>> {
    let scope = DataScope::from_str(&args.scope)?;
    let date = parse_date(&args.date)?;
    let credentials = TdxCredentials::from_env()?;
    let report = fetch_raw_snapshot_with_progress(
        scope,
        date,
        &args.output,
        credentials,
        print_fetch_progress,
    )
    .await?;
    let snapshot = args.output.join("raw").join(&report.manifest.date);
    println!(
        "reused={} downloaded={} total={}",
        report.reused_count,
        report.downloaded_count,
        report.manifest.sources.len()
    );
    println!("snapshot: {}", snapshot.display());
    Ok(())
}

fn print_fetch_progress(progress: FetchProgress) {
    match progress {
        FetchProgress::Reuse {
            source_key,
            relative_path,
        } => println!("REUSE  {source_key}        {relative_path}"),
        FetchProgress::Fetch { source_key } => println!("FETCH  {source_key}"),
        FetchProgress::Wait { duration } => {
            println!("WAIT   {} for TDX rate limit", duration_text(duration))
        }
        FetchProgress::Retry {
            source_key,
            duration,
        } => println!(
            "RETRY  {source_key} after {} (429)",
            duration_text(duration)
        ),
    }
}

fn duration_text(duration: std::time::Duration) -> String {
    if duration.subsec_millis() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}.{:03}s", duration.as_secs(), duration.subsec_millis())
    }
}

async fn build(args: BuildArgs) -> Result<(), Box<dyn Error>> {
    let scope = args.scope.as_deref().map(DataScope::from_str).transpose()?;
    if args.street_router.is_some() && !args.preprocess_transfers {
        return Err("--street-router requires --preprocess-transfers".into());
    }
    let result = if args.preprocess_transfers {
        let street = build_street_router(args.street_router)?;
        let progress_bar = transfer_progress_bar();
        let mut progress_started = false;
        let mut update_progress = |progress: TransferPreprocessingProgress| {
            let start_progress = !progress_started;
            if start_progress {
                progress_bar.reset_elapsed();
                progress_started = true;
            }
            progress_bar.set_length(progress.total_origin_stops as u64);
            progress_bar.set_position(progress.completed_origin_stops as u64);
            progress_bar.set_message(format!("shortcuts={}", progress.transfer_shortcuts));
            if start_progress {
                progress_bar.set_draw_target(transfer_progress_draw_target());
                progress_bar.enable_steady_tick(Duration::from_millis(250));
            }
        };
        let result = build_from_raw_with_transfer_preprocessing_with_progress(
            &args.input,
            &args.output,
            scope,
            args.date,
            street,
            RoutingLimits::default(),
            &mut update_progress,
        )
        .await;
        match result {
            Ok(result) => {
                progress_bar.finish();
                result
            }
            Err(error) => {
                progress_bar.abandon();
                return Err(error.into());
            }
        }
    } else {
        build_from_raw(&args.input, &args.output, scope, args.date)?
    };
    print_build_diagnostics(&result.diagnostics);
    print_counts("built", &result.manifest.counts, &result.manifest.revisions);
    println!(
        "network artifact: {}",
        args.output.join("transit-network.json").display()
    );
    println!(
        "transfer artifact: {}",
        args.output.join("transfer-shortcuts.json").display()
    );
    println!("manifest: {}", args.output.join("manifest.json").display());
    Ok(())
}

fn transfer_progress_bar() -> ProgressBar {
    let progress_bar = ProgressBar::with_draw_target(Some(0), ProgressDrawTarget::hidden());
    progress_bar.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] [{bar:20}] {pos}/{len} {percent:>3}% ETA {eta} {msg}",
        )
        .expect("transfer preprocessing progress style is valid")
        .progress_chars("█░"),
    );
    progress_bar
}

fn transfer_progress_draw_target() -> ProgressDrawTarget {
    if std::io::stderr().is_terminal() {
        ProgressDrawTarget::stderr_with_hz(10)
    } else {
        ProgressDrawTarget::hidden()
    }
}

fn build_street_router(
    requested: Option<BuildStreetRouter>,
) -> Result<Arc<dyn StreetRouter>, Box<dyn Error>> {
    let requested = requested.unwrap_or_else(|| {
        if env::var_os("VIA_VALHALLA_GRAPH_REVISION").is_some()
            || env::var_os("VIA_VALHALLA_TILESET_LAST_MODIFIED").is_some()
        {
            BuildStreetRouter::Valhalla
        } else {
            BuildStreetRouter::StraightLine
        }
    });
    match requested {
        BuildStreetRouter::StraightLine => Ok(Arc::new(StraightLineStreetRouter::default())),
        BuildStreetRouter::Valhalla => {
            let graph_revision = env::var("VIA_VALHALLA_GRAPH_REVISION")
                .map_err(|_| "Valhalla preprocessing requires VIA_VALHALLA_GRAPH_REVISION")?;
            let tileset_last_modified = env::var("VIA_VALHALLA_TILESET_LAST_MODIFIED")
                .map_err(|_| "Valhalla preprocessing requires VIA_VALHALLA_TILESET_LAST_MODIFIED")?
                .parse()?;
            let mut config = ValhallaConfig::local(graph_revision, tileset_last_modified);
            if let Ok(base_url) = env::var("VIA_VALHALLA_URL") {
                config.base_url = url::Url::parse(&base_url)?;
            }
            Ok(Arc::new(ValhallaProvider::new(config)?))
        }
    }
}

fn validate(args: ValidateArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact)?;
    let transfer_path = args
        .transfer_artifact
        .unwrap_or_else(|| args.artifact.with_file_name("transfer-shortcuts.json"));
    let transfer_count = if transfer_path.exists() {
        let index = load_transfer_artifact(&transfer_path)?;
        index
            .validate_against(&network)
            .map_err(DataError::InvalidTransferArtifact)?;
        index.len()
    } else {
        network.transfer_edges.len()
    };
    let counts = ArtifactCounts {
        stops: network.stops.len(),
        services: network.services.len(),
        scheduled_trips: network.trips.len(),
        bike_stations: network.bike_share_stations.len(),
        transfer_shortcuts: transfer_count,
    };
    print_counts(
        "validated",
        &counts,
        &via_server::data::ArtifactRevisions {
            static_network_revision: network.static_network_revision.clone(),
            schedule_revision: network.schedule_revision.clone(),
        },
    );
    Ok(())
}

fn inspect(args: InspectArgs) -> Result<(), Box<dyn Error>> {
    match args.command {
        InspectCommand::Stop(args) => inspect_stop(args),
        InspectCommand::Service(args) => inspect_service(args),
        InspectCommand::NearbyStops(args) => inspect_nearby_stops(args),
        InspectCommand::BikeNear(args) => inspect_bike_near(args),
        InspectCommand::Stats(args) => inspect_stats(args),
    }
}

fn inspect_stop(args: StopArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact.artifact)?;
    let query = args.query.to_lowercase();
    let mut matches = network
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
        for service in &network.services {
            if let Some(position) = service.pattern.iter().position(|id| id == &stop.id) {
                println!(
                    "  service={} label={:?} position={}",
                    service.id.as_str(),
                    service.label,
                    position + 1
                );
            }
        }
    }
    Ok(())
}

fn inspect_service(args: ServiceArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact.artifact)?;
    let query = args.query.to_lowercase();
    let mut matches = network
        .services
        .iter()
        .filter(|service| {
            service.id.as_str() == args.query || service.label.to_lowercase().contains(&query)
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    if matches.is_empty() {
        println!("no services matched {:?}", args.query);
        return Ok(());
    }
    for service in matches {
        println!(
            "service={} label={:?} mode={} timing_source={:?} headsign={:?}",
            service.id.as_str(),
            service.label,
            mode_name(service.transit_mode),
            service.timing_source,
            service.headsign
        );
        println!(
            "  stops: {}",
            service
                .pattern
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        );
        println!(
            "  scheduled trips: {}",
            network
                .trips
                .iter()
                .filter(|trip| trip.service_id == service.id)
                .count()
        );
    }
    Ok(())
}

fn inspect_nearby_stops(args: NearbyArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact.artifact)?;
    let coordinate = coordinate(args.lat, args.lon)?;
    for stop in network.nearest_stops(coordinate, args.limit.max(1)) {
        println!(
            "{:.0}m  {}  {}  {}",
            distance_metres(
                args.lat,
                args.lon,
                stop.coordinate.latitude,
                stop.coordinate.longitude
            ),
            stop.id.as_str(),
            stop.name,
            service_labels(&network, &stop.id)
        );
    }
    Ok(())
}

fn inspect_bike_near(args: NearbyArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact.artifact)?;
    let coordinate = coordinate(args.lat, args.lon)?;
    for station in network.nearest_bike_share_stations(coordinate, args.limit.max(1)) {
        println!(
            "{:.0}m  {}  {} ({:.6}, {:.6})",
            distance_metres(
                args.lat,
                args.lon,
                station.coordinate.latitude,
                station.coordinate.longitude
            ),
            station.id.as_str(),
            station.name,
            station.coordinate.latitude,
            station.coordinate.longitude
        );
    }
    Ok(())
}

fn inspect_stats(args: StatsArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact.artifact)?;
    let manifest_path = args.artifact.artifact.with_file_name("manifest.json");
    let counts = load_manifest(&manifest_path)
        .ok()
        .map(|manifest| manifest.counts)
        .unwrap_or_else(|| ArtifactCounts {
            stops: network.stops.len(),
            services: network.services.len(),
            scheduled_trips: network.trips.len(),
            bike_stations: network.bike_share_stations.len(),
            transfer_shortcuts: network.transfer_edges.len(),
        });
    println!("network revision: {}", network.static_network_revision);
    println!("schedule revision: {}", network.schedule_revision);
    println!("stop count: {}", counts.stops);
    println!("service count: {}", counts.services);
    println!("scheduled trip count: {}", counts.scheduled_trips);
    for mode in [
        TransitMode::Bus,
        TransitMode::Metro,
        TransitMode::Rail,
        TransitMode::HighSpeedRail,
    ] {
        println!(
            "{} service count: {}",
            mode_name(mode),
            network
                .services
                .iter()
                .filter(|service| service.transit_mode == mode)
                .count()
        );
    }
    println!("BikeShare station count: {}", counts.bike_stations);
    println!("transfer shortcut count: {}", counts.transfer_shortcuts);
    println!("service-day boundary: {}", network.service_day_boundary);
    Ok(())
}

fn print_counts(
    prefix: &str,
    counts: &ArtifactCounts,
    revisions: &via_server::data::ArtifactRevisions,
) {
    println!("{prefix} artifact");
    println!(
        "  static network revision: {}",
        revisions.static_network_revision
    );
    println!("  schedule revision: {}", revisions.schedule_revision);
    println!(
        "  stops={} services={} scheduled_trips={} bike_stations={} transfer_shortcuts={}",
        counts.stops,
        counts.services,
        counts.scheduled_trips,
        counts.bike_stations,
        counts.transfer_shortcuts
    );
}

fn print_build_diagnostics(diagnostics: &BuildDiagnostics) {
    for skipped in &diagnostics.skipped_services {
        println!(
            "SKIP service={} reason={} stops={}",
            skipped.service_id, skipped.reason, skipped.stops
        );
    }
    println!(
        "skipped malformed/degenerate services: {}",
        diagnostics.skipped_services.len()
    );
    if diagnostics.malformed_bus_records > 0
        || diagnostics.unresolved_bus_stop_references > 0
        || diagnostics.duplicate_bus_stop_artifacts_removed > 0
    {
        println!(
            "normalization diagnostics: malformed_bus_records={} unresolved_bus_stop_references={} duplicate_bus_stop_artifacts_removed={}",
            diagnostics.malformed_bus_records,
            diagnostics.unresolved_bus_stop_references,
            diagnostics.duplicate_bus_stop_artifacts_removed
        );
    }
}

fn parse_date(value: &str) -> Result<Date, DataError> {
    let format = time::format_description::parse("[year]-[month]-[day]")
        .map_err(|_| DataError::InvalidDate(value.into()))?;
    Date::parse(value, &format).map_err(|_| DataError::InvalidDate(value.into()))
}

fn coordinate(
    latitude: f64,
    longitude: f64,
) -> Result<via_server::api::RequestCoordinate, DataError> {
    if !latitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !longitude.is_finite()
        || !(-180.0..=180.0).contains(&longitude)
    {
        return Err(DataError::InvalidCoordinate(format!(
            "invalid coordinate {latitude},{longitude}"
        )));
    }
    Ok(via_server::api::RequestCoordinate {
        latitude,
        longitude,
    })
}

fn service_labels(network: &TransitNetwork, stop_id: &BoardingPointId) -> String {
    network
        .services
        .iter()
        .filter(|service| service.pattern.iter().any(|id| id == stop_id))
        .map(|service| service.label.as_str())
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

fn distance_metres(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    let radius = 6_371_000.0;
    let lat1 = from_lat.to_radians();
    let lat2 = to_lat.to_radians();
    let dlat = (to_lat - from_lat).to_radians();
    let dlon = (to_lon - from_lon).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    radius * 2.0 * a.sqrt().asin()
}
