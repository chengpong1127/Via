use std::{error::Error, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use clap::{Args, Parser, Subcommand};
use time::Date;
use via_server::{
    config::{TdxCredentials, load_dotenv},
    data::{
        ArtifactCounts, BuildDiagnostics, DataError, DataScope, FetchProgress,
        StaticTransferPolicy, build_from_raw_with_street, fetch_raw_snapshot_with_progress,
        load_manifest, load_network_artifact,
    },
    domain::{
        ids::BoardingPointId,
        transit::{TransitMode, TransitNetwork},
    },
    health::DependencyHealth,
    providers::{ValhallaConfig, ValhallaProvider},
    street::StreetRouter,
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
}

#[derive(Args, Debug)]
struct ValidateArgs {
    #[arg(long)]
    artifact: PathBuf,
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
    let street = build_street_router().await?;
    let result = build_from_raw_with_street(
        &args.input,
        &args.output,
        scope,
        args.date,
        street.as_ref(),
        StaticTransferPolicy::default(),
    )
    .await?;
    print_build_diagnostics(&result.diagnostics);
    print_counts("built", &result.manifest.counts, &result.manifest.revisions);
    println!(
        "network artifact: {}",
        args.output.join("transit-network.json").display()
    );
    println!("manifest: {}", args.output.join("manifest.json").display());
    Ok(())
}

async fn build_street_router() -> Result<Arc<dyn StreetRouter>, Box<dyn Error>> {
    let graph_revision = std::env::var("VIA_VALHALLA_GRAPH_REVISION")?;
    let tileset_last_modified = std::env::var("VIA_VALHALLA_TILESET_LAST_MODIFIED")?.parse()?;
    let mut config = ValhallaConfig::local(graph_revision, tileset_last_modified);
    if let Ok(base_url) = std::env::var("VIA_VALHALLA_URL") {
        config.base_url = url::Url::parse(&base_url)?;
    }
    // Static discovery evaluates many bounded matrices. Keep the request
    // concurrency bounded and allow a slower matrix to finish publication;
    // runtime routing retains the stricter provider default.
    config.request_timeout = Duration::from_secs(5);
    let provider = ValhallaProvider::new(config)?;
    if !provider.healthy().await {
        return Err("Valhalla is not healthy for the configured graph artifact".into());
    }
    Ok(Arc::new(provider))
}

fn validate(args: ValidateArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact)?;
    let counts = ArtifactCounts {
        stops: network.stops.len(),
        services: network.services.len(),
        scheduled_trips: network.trips.len(),
        bike_stations: network.bike_share_stations.len(),
        transfer_edges: network.transfer_edges.len(),
        bike_share_connectors: network.bike_share_connectors.len(),
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
            coordinate.haversine_distance_metres(stop.coordinate),
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
            coordinate.haversine_distance_metres(station.coordinate),
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
            transfer_edges: network.transfer_edges.len(),
            bike_share_connectors: network.bike_share_connectors.len(),
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
    println!("transfer edge count: {}", counts.transfer_edges);
    println!(
        "Transit↔BikeShare connector count: {}",
        counts.bike_share_connectors
    );
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
        "  stops={} services={} scheduled_trips={} bike_stations={} transfer_edges={} bike_share_connectors={}",
        counts.stops,
        counts.services,
        counts.scheduled_trips,
        counts.bike_stations,
        counts.transfer_edges,
        counts.bike_share_connectors
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
    if let Some(transfer) = &diagnostics.transfer_discovery {
        println!(
            "static transfer discovery: boarding_points={} source_points_processed={} geometric_candidate_pairs={} matrix_requests={} reachable_walking_pairs={} inferred_edges_kept={} explicit_edges_kept={} total_transfer_edges={} wall_clock_ms={}",
            transfer.boarding_points,
            transfer.source_points_processed,
            transfer.geometric_candidate_pairs,
            transfer.matrix_requests,
            transfer.reachable_walking_pairs,
            transfer.inferred_edges_kept,
            transfer.explicit_edges_kept,
            transfer.total_transfer_edges,
            transfer.build_wall_clock_milliseconds,
        );
        if let Some(bike) = &transfer.bike_share_connectors {
            println!(
                "static BikeShare connector discovery: boarding_points={} geometric_candidate_pairs={} matrix_requests={} reachable_directions={} connectors_kept={}",
                bike.boarding_points,
                bike.geometric_candidate_pairs,
                bike.matrix_requests,
                bike.reachable_directions,
                bike.connectors_kept,
            );
        }
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
