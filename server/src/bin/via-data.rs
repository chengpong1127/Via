use std::{
    env,
    error::Error,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use clap::{Args, Parser, Subcommand};
use via_server::{
    config::load_dotenv,
    data::{
        ArtifactCounts, GtfsBuildDiagnostics, build_from_gtfs_archive_with_street,
        build_from_gtfs_archive_with_tdx_bike_stations, load_bikeshare_artifact, load_manifest,
        load_network_artifact,
    },
    domain::{ids::BoardingPointId, transit::TransitMode, transit_runtime::TransitRuntime},
    providers::valhalla::{ValhallaConfig, ValhallaProvider},
};

#[derive(Parser, Debug)]
#[command(
    name = "via-data",
    about = "Build, validate, and inspect Via GTFS data artifacts"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Build(BuildArgs),
    Validate(ValidateArgs),
    Inspect(InspectArgs),
}

#[derive(Args, Debug)]
struct BuildArgs {
    /// Local GTFS .zip archive. Valhalla is queried for walking transfers.
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    /// Local TDX `Bike/Station/City/Taipei` JSON snapshot.
    #[arg(long)]
    bike_stations_taipei: Option<PathBuf>,
    /// Local TDX `Bike/Station/City/NewTaipei` JSON snapshot.
    #[arg(long)]
    bike_stations_new_taipei: Option<PathBuf>,
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
    Pattern(PatternArgs),
    NearbyStops(NearbyArgs),
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
struct PatternArgs {
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

fn main() -> Result<(), Box<dyn Error>> {
    load_dotenv();
    let cli = Cli::parse();
    match cli.command {
        Command::Build(args) => build(args)?,
        Command::Validate(args) => validate(args)?,
        Command::Inspect(args) => inspect(args)?,
    }
    Ok(())
}

fn build(args: BuildArgs) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let mut config = ValhallaConfig::local();
    config.request_timeout = Duration::from_secs(15);
    if let Ok(base_url) = env::var("VIA_VALHALLA_URL") {
        config.base_url = url::Url::parse(&base_url)?;
    }
    let street = ValhallaProvider::new(config)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = match (&args.bike_stations_taipei, &args.bike_stations_new_taipei) {
        (Some(taipei), Some(new_taipei)) => {
            runtime.block_on(build_from_gtfs_archive_with_tdx_bike_stations(
                &args.input,
                &args.output,
                taipei,
                new_taipei,
                &street,
            ))?
        }
        (None, None) => runtime.block_on(build_from_gtfs_archive_with_street(
            &args.input,
            &args.output,
            &street,
        ))?,
        _ => {
            return Err(
                "provide both --bike-stations-taipei and --bike-stations-new-taipei".into(),
            );
        }
    };
    println!("build latency: {} ms", started.elapsed().as_millis());
    print_build_diagnostics(&result.diagnostics, &result.build.diagnostics);
    print_counts(
        "built",
        &result.build.manifest.counts,
        &result.build.manifest.revisions,
    );
    println!(
        "network artifact: {}",
        args.output.join("transit-network.json").display()
    );
    println!(
        "TDX bindings artifact: {}",
        args.output.join("provider-bindings.json").display()
    );
    println!(
        "bike-share artifact: {}",
        args.output.join("bikeshare-network.json").display()
    );
    println!("manifest: {}", args.output.join("manifest.json").display());
    Ok(())
}

fn validate(args: ValidateArgs) -> Result<(), Box<dyn Error>> {
    let network = load_network_artifact(&args.artifact)?;
    let bike_share_network =
        load_bikeshare_artifact(&args.artifact.with_file_name("bikeshare-network.json"))?;
    let counts = ArtifactCounts {
        stops: network.stops.len(),
        routes: network.routes.len(),
        patterns: network.patterns.len(),
        trips: network.trips.len(),
        calendars: network.calendars.len(),
        frequencies: network.frequencies.len(),
        transfer_edges: network.transfer_edges.len(),
        bike_stations: bike_share_network.stations.len(),
        bike_access_links: bike_share_network.access_links.len(),
    };
    print_counts(
        "validated",
        &counts,
        &via_server::data::ArtifactRevisions {
            static_network_revision: network.static_network_revision.clone(),
            schedule_revision: network.schedule_revision.clone(),
            bike_share_network_revision: bike_share_network.static_network_revision.clone(),
        },
    );
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
    let network = Arc::new(load_network_artifact(&args.artifact.artifact)?);
    let network = TransitRuntime::new(network);
    let data = network.data();
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
                    network.route_of(pattern).label,
                    position + 1
                );
            }
        }
    }
    Ok(())
}

fn inspect_pattern(args: PatternArgs) -> Result<(), Box<dyn Error>> {
    let network = Arc::new(load_network_artifact(&args.artifact.artifact)?);
    let network = TransitRuntime::new(network);
    let data = network.data();
    let query = args.query.to_lowercase();
    let mut matches = data
        .patterns
        .iter()
        .filter(|pattern| {
            pattern.id.as_str() == args.query
                || network
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
            network.route_of(pattern).label,
            mode_name(network.route_of(pattern).mode),
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
    let network = Arc::new(load_network_artifact(&args.artifact.artifact)?);
    let network = TransitRuntime::new(network);
    let coordinate = coordinate(args.lat, args.lon)?;
    for stop in network.nearest_stops(coordinate, args.limit.max(1)) {
        println!(
            "{:.0}m  {}  {}  {}",
            coordinate.haversine_distance_metres(stop.coordinate),
            stop.id.as_str(),
            stop.name,
            pattern_labels(&network, &stop.id)
        );
    }
    Ok(())
}

fn inspect_stats(args: StatsArgs) -> Result<(), Box<dyn Error>> {
    let network = Arc::new(load_network_artifact(&args.artifact.artifact)?);
    let network = TransitRuntime::new(network);
    let data = network.data();
    let bike_share_network = load_bikeshare_artifact(
        &args
            .artifact
            .artifact
            .with_file_name("bikeshare-network.json"),
    )
    .ok();
    let manifest_path = args.artifact.artifact.with_file_name("manifest.json");
    let counts = load_manifest(&manifest_path)
        .ok()
        .map(|manifest| manifest.counts)
        .unwrap_or_else(|| ArtifactCounts {
            stops: data.stops.len(),
            routes: data.routes.len(),
            patterns: data.patterns.len(),
            trips: data.trips.len(),
            calendars: data.calendars.len(),
            frequencies: data.frequencies.len(),
            transfer_edges: data.transfer_edges.len(),
            bike_stations: bike_share_network
                .as_ref()
                .map_or(0, |network| network.stations.len()),
            bike_access_links: bike_share_network
                .as_ref()
                .map_or(0, |network| network.access_links.len()),
        });
    println!("network revision: {}", data.static_network_revision);
    println!("schedule revision: {}", data.schedule_revision);
    if let Some(bike_share_network) = &bike_share_network {
        println!(
            "bike-share artifact revision: {}",
            bike_share_network.static_network_revision
        );
    }
    println!("stop count: {}", counts.stops);
    println!("route count: {}", counts.routes);
    println!("pattern count: {}", counts.patterns);
    println!("trip count: {}", counts.trips);
    println!("calendar count: {}", counts.calendars);
    println!("frequency count: {}", counts.frequencies);
    println!(
        "derived Bus Phase 1 profiles: {}",
        data.patterns
            .iter()
            .filter(|pattern| pattern
                .static_profile
                .segments
                .iter()
                .all(|segment| segment.source
                    == via_server::domain::transit::SegmentTimeSource::Derived))
            .count()
    );
    for mode in [
        TransitMode::Bus,
        TransitMode::Metro,
        TransitMode::Rail,
        TransitMode::HighSpeedRail,
    ] {
        println!(
            "{} pattern count: {}",
            mode_name(mode),
            data.patterns
                .iter()
                .filter(|pattern| network.route_of(pattern).mode == mode)
                .count()
        );
    }
    println!("transfer edge count: {}", counts.transfer_edges);
    println!("bike station count: {}", counts.bike_stations);
    println!("bike access link count: {}", counts.bike_access_links);
    println!("service-day boundary: {}", data.service_day_boundary);
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
        "  bike-share network revision: {}",
        revisions.bike_share_network_revision
    );
    println!(
        "  stops={} routes={} patterns={} trips={} calendars={} frequencies={} transfer_edges={} bike_stations={} bike_access_links={}",
        counts.stops,
        counts.routes,
        counts.patterns,
        counts.trips,
        counts.calendars,
        counts.frequencies,
        counts.transfer_edges,
        counts.bike_stations,
        counts.bike_access_links
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
        for (modes, count) in &stats.by_mode {
            println!("  {modes}: {count}");
        }
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
    for id in &diagnostics.invalid_timing_trip_examples {
        println!("  trip: {id}");
    }
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
    for id in &diagnostics.unresolved_bus_realtime_scope_examples {
        println!("  pattern: {id}");
    }
    println!(
        "GTFS stop IDs without current TDX ID mapping: {}",
        diagnostics.gtfs_stop_ids_without_tdx_mapping
    );
    for id in &diagnostics.unmapped_stop_id_examples {
        println!("  stop: {id}");
    }
    println!(
        "Via Pattern IDs without current TDX realtime mapping: {}",
        diagnostics.gtfs_pattern_ids_without_tdx_mapping
    );
    for id in &diagnostics.unmapped_pattern_id_examples {
        println!("  pattern: {id}");
    }
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
