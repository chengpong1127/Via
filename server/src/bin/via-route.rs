use std::{error::Error, path::PathBuf};

use clap::Parser;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    api::{DepartureSetting, PlanningMode, PlanningRequest, SelectedPlace},
    app::{profiling, runtime::build_runtime},
    config::{Environment, RuntimeConfig, ViaConfig, load_dotenv},
    data::load_static_artifact,
    domain::{journey::PlanningOutcome, realtime::FreshnessState},
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
    #[arg(long, value_delimiter = ',')]
    modes: Vec<PlanningMode>,
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
    let _trace_guard = profiling::init(log_filter)?;
    let environment = Environment::from_env_or_development()?;
    let settings = ViaConfig::load_for_with(cli.config.as_deref(), environment, |settings| {
        apply_cli_overrides(&cli, settings)
    })?;
    let max_static_corridors = settings.routing.max_static_corridors;
    let via_static = load_static_artifact(&cli.static_path)?;
    let static_revision = via_static.revisions.transit_static.clone();
    let schedule_revision = via_static.revisions.schedule.clone();
    let runtime_config = RuntimeConfig::from_settings(environment, settings)?;
    let runtime = build_runtime(via_static, runtime_config).await?;
    let request = request_from_cli(&cli)?;
    if cli.trace {
        eprintln!("  max static corridors={max_static_corridors}");
        eprintln!("ACCESS");
        eprintln!("  origin ({:.6}, {:.6})", cli.from_lat, cli.from_lon);
        eprintln!("  destination ({:.6}, {:.6})", cli.to_lat, cli.to_lon);
        let modes = cli
            .modes
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        let modes = modes
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(",");
        eprintln!(
            "  modes={}",
            if modes.is_empty() {
                "walking only"
            } else {
                &modes
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

fn apply_cli_overrides(cli: &Cli, settings: &mut ViaConfig) {
    if let Some(value) = cli.max_departures_per_query {
        settings.routing.max_departures_per_query = value;
    }
    if let Some(value) = cli.max_static_corridors {
        settings.routing.max_static_corridors = value;
    }
    if let Some(value) = cli.valhalla_timeout_ms {
        settings.valhalla.request_timeout_ms = value;
        settings.server.request_timeout_ms = value.saturating_mul(5);
    }
}

fn request_from_cli(cli: &Cli) -> Result<PlanningRequest, Box<dyn Error>> {
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
        selected_modes: cli.modes.clone(),
        departure_setting,
    };
    request
        .validate()
        .map_err(|error| Box::new(error) as Box<dyn Error>)?;
    Ok(request)
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
    use super::{Cli, apply_cli_overrides, request_from_cli};
    use clap::Parser;
    use via_server::{api::PlanningMode, config::Environment};

    const REQUIRED: [&str; 11] = [
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
    ];

    fn parse(extra: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(REQUIRED.iter().chain(extra))
    }

    #[test]
    fn explicit_cli_values_override_toml_and_preserve_timeout_scaling() {
        let cli = parse(&[
            "--max-static-corridors",
            "7",
            "--max-departures-per-query",
            "9",
            "--valhalla-timeout-ms",
            "2200",
        ])
        .unwrap();
        let mut config = via_server::config::ViaConfig::default();
        apply_cli_overrides(&cli, &mut config);
        config.validate().unwrap();
        let limits = config.journey_limits(Environment::Production);
        assert_eq!(limits.planner.max_static_corridors, 7);
        assert_eq!(limits.planner.max_departures_per_query, 9);
        assert_eq!(
            config
                .valhalla
                .provider_config(false)
                .unwrap()
                .request_timeout,
            std::time::Duration::from_millis(2200)
        );
        assert_eq!(
            limits.request_timeout,
            Some(std::time::Duration::from_secs(11))
        );
    }

    #[test]
    fn omitted_modes_selects_walking_only() {
        assert!(parse(&[]).unwrap().modes.is_empty());
    }

    #[test]
    fn accepts_bike_share_with_transit_modes() {
        let modes = parse(&["--modes", "bus,metro,bike_share"]).unwrap().modes;
        assert_eq!(
            modes,
            [
                PlanningMode::Bus,
                PlanningMode::Metro,
                PlanningMode::BikeShare
            ]
        );
    }

    #[test]
    fn rejects_duplicate_and_unknown_modes() {
        assert!(request_from_cli(&parse(&["--modes", "bus,bus"]).unwrap()).is_err());
        assert!(parse(&["--modes", "walk"]).is_err());
        assert!(parse(&["--modes", "bus,,metro"]).is_err());
    }
}
