use std::{error::Error, path::PathBuf, time::Duration};

use clap::Parser;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    api::{DepartureSetting, PlanningMode, PlanningRequest, SelectedPlace},
    config::{Environment, RuntimeConfig, ViaConfig, load_dotenv},
    data::load_static_artifact,
    domain::{journey::PlanningOutcome, realtime::FreshnessState},
    runtime::{RuntimeOptions, build_runtime},
};

#[derive(Parser, Debug)]
#[command(name = "via-route", about = "Run a live Via routing query")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long = "static")]
    static_path: PathBuf,
    #[arg(long)]
    from_lat: f64,
    #[arg(long)]
    from_lon: f64,
    #[arg(long)]
    to_lat: f64,
    #[arg(long)]
    to_lon: f64,
    #[arg(long, conflicts_with = "depart_at")]
    depart_now: bool,
    #[arg(long)]
    depart_at: Option<String>,
    #[arg(long)]
    modes: Option<String>,
    #[arg(long)]
    max_departures_per_query: Option<usize>,
    #[arg(long)]
    max_static_corridors: Option<usize>,
    #[arg(long)]
    valhalla_timeout_ms: Option<u64>,
    #[arg(long)]
    trace: bool,
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    load_dotenv();
    let cli = Cli::parse();
    let log_filter = cli.trace.then(|| {
        tracing_subscriber::EnvFilter::new(
            "via_server::routing=debug,via_server::providers::timing=debug",
        )
    });
    let _trace_guard = via_server::profiling::init(log_filter)?;
    let mut settings = ViaConfig::load(cli.config.as_deref())?;
    settings.apply_environment()?;
    let options = options_from_cli(&cli, &mut settings)?;
    let max_static_corridors = settings.routing.max_static_corridors;
    let via_static = load_static_artifact(&cli.static_path)?;
    let static_revision = via_static.revisions.transit_static.clone();
    let schedule_revision = via_static.revisions.schedule.clone();
    let runtime_config =
        RuntimeConfig::from_settings(Environment::from_env_or_development()?, settings)?;
    let runtime = build_runtime(via_static, options, runtime_config).await?;
    let request = request_from_cli(&cli)?;
    if cli.trace {
        eprintln!("  max static corridors={max_static_corridors}");
        eprintln!("ACCESS");
        eprintln!("  origin ({:.6}, {:.6})", cli.from_lat, cli.from_lon);
        eprintln!("  destination ({:.6}, {:.6})", cli.to_lat, cli.to_lon);
        let modes = cli.modes.as_deref().unwrap_or_default();
        eprintln!(
            "  modes={}",
            if modes.trim().is_empty() {
                "walking only"
            } else {
                modes
            }
        );
        eprintln!("  static artifact={}", cli.static_path.display());
        eprintln!("  transit revision={static_revision}");
    }
    let result = runtime
        .routing
        .plan(request.into())
        .await
        .map_err(|error| format!("routing failed: {error:?}"))?;
    if cli.trace {
        print_trace(&result);
    }
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_summary(&result, &static_revision, &schedule_revision);
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

fn options_from_cli(
    cli: &Cli,
    settings: &mut ViaConfig,
) -> Result<RuntimeOptions, via_server::config::ConfigError> {
    if let Some(value) = cli.max_departures_per_query {
        settings.routing.max_departures_per_query = value;
    }
    if let Some(value) = cli.max_static_corridors {
        settings.routing.max_static_corridors = value;
    }
    if let Some(value) = cli.valhalla_timeout_ms {
        settings.valhalla.request_timeout_ms = value;
    }
    settings.validate()?;
    let mut limits = settings.journey_limits();
    let valhalla_timeout = cli.valhalla_timeout_ms.map(Duration::from_millis);
    if let Some(timeout) = valhalla_timeout {
        limits.request_timeout = timeout.saturating_mul(5);
    }
    Ok(RuntimeOptions {
        limits: Some(limits),
        valhalla_request_timeout: valhalla_timeout,
    })
}

fn request_from_cli(cli: &Cli) -> Result<PlanningRequest, Box<dyn Error>> {
    let selected_modes = parse_modes(cli.modes.as_deref())?;
    let departure_setting = if let Some(value) = &cli.depart_at {
        DepartureSetting::DepartAt {
            time: parse_time(value)?,
        }
    } else {
        let _ = cli.depart_now;
        DepartureSetting::DepartNow
    };
    let request = PlanningRequest {
        origin: SelectedPlace {
            coordinate: via_server::api::GeoCoordinate {
                latitude: cli.from_lat,
                longitude: cli.from_lon,
            },
            display_text: "origin".into(),
        },
        destination: SelectedPlace {
            coordinate: via_server::api::GeoCoordinate {
                latitude: cli.to_lat,
                longitude: cli.to_lon,
            },
            display_text: "destination".into(),
        },
        selected_modes,
        departure_setting,
    };
    request
        .validate()
        .map_err(|error| Box::new(error) as Box<dyn Error>)?;
    Ok(request)
}

fn parse_modes(value: Option<&str>) -> Result<Vec<PlanningMode>, Box<dyn Error>> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let mut modes = value
        .split(',')
        .map(str::trim)
        .map(|mode| match mode {
            "bus" => Ok(PlanningMode::Bus),
            "metro" => Ok(PlanningMode::Metro),
            "tra" => Ok(PlanningMode::Tra),
            "thsr" => Ok(PlanningMode::Thsr),
            "bike_share" => Ok(PlanningMode::BikeShare),
            _ => Err(format!(
                "unsupported mode {mode:?}; use bus, metro, tra, thsr, or bike_share"
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    modes.sort();
    if modes.windows(2).any(|modes| modes[0] == modes[1]) {
        return Err("routing modes must not contain duplicates".into());
    }
    Ok(modes)
}

fn parse_time(value: &str) -> Result<OffsetDateTime, Box<dyn Error>> {
    Ok(OffsetDateTime::parse(value, &Rfc3339)?)
}

fn print_summary(result: &PlanningOutcome, network_revision: &str, schedule_revision: &str) {
    println!("network revision: {network_revision}");
    println!("schedule revision: {schedule_revision}");
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
        via_server::domain::journey::LegDetails::Walking => {
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

fn print_trace(result: &PlanningOutcome) {
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
                        via_server::domain::journey::LegDetails::Walking => eprintln!(
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

#[cfg(test)]
mod tests {
    use super::parse_modes;
    use via_server::api::PlanningMode;

    #[test]
    fn explicit_cli_values_override_toml_and_preserve_timeout_scaling() {
        use clap::Parser;
        let cli = super::Cli::parse_from([
            "via-route",
            "--static",
            "fixture.json",
            "--from-lat",
            "25",
            "--from-lon",
            "121.5",
            "--to-lat",
            "25.1",
            "--to-lon",
            "121.6",
            "--max-static-corridors",
            "7",
            "--max-departures-per-query",
            "9",
            "--valhalla-timeout-ms",
            "2200",
        ]);
        let mut config = via_server::config::ViaConfig::default();
        let options = super::options_from_cli(&cli, &mut config).unwrap();
        assert_eq!(
            options
                .limits
                .as_ref()
                .unwrap()
                .planner
                .max_static_corridors,
            7
        );
        assert_eq!(
            options
                .limits
                .as_ref()
                .unwrap()
                .planner
                .max_departures_per_query,
            9
        );
        assert_eq!(
            options.valhalla_request_timeout,
            Some(std::time::Duration::from_millis(2200))
        );
        assert_eq!(
            options.limits.as_ref().unwrap().request_timeout,
            std::time::Duration::from_secs(11)
        );
    }

    #[test]
    fn omitted_or_empty_modes_selects_walking_only() {
        assert!(parse_modes(None).unwrap().is_empty());
        assert!(parse_modes(Some("")).unwrap().is_empty());
        assert!(parse_modes(Some("  ")).unwrap().is_empty());
    }

    #[test]
    fn accepts_bike_share_with_transit_modes() {
        let modes = parse_modes(Some("bus,metro,bike_share")).unwrap();
        assert_eq!(modes.len(), 3);
        assert!(modes.contains(&PlanningMode::BikeShare));
    }

    #[test]
    fn rejects_duplicate_and_unknown_modes() {
        assert!(parse_modes(Some("bus,bus")).is_err());
        assert!(parse_modes(Some("walk")).is_err());
        assert!(parse_modes(Some("bus,,metro")).is_err());
    }
}
