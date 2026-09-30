use std::{error::Error, path::PathBuf, time::Duration};

use clap::Parser;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use via_server::{
    api::{DepartureSetting, PlanningMode, PlanningRequest, SelectedPlace, new_request_id},
    config::load_dotenv,
    data::load_static_artifact,
    domain::{journey::PlanningOutcome, realtime::FreshnessState},
    routing::RoutingLimits,
    runtime::{ProductionRuntimeOptions, build_production_runtime},
};

#[derive(Parser, Debug)]
#[command(name = "via-route", about = "Run a live Via routing query")]
struct Cli {
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
    if cli.trace {
        tracing_subscriber::fmt()
            .with_env_filter("via_server::routing=debug,via_server::providers::timing=debug")
            .without_time()
            .init();
    }
    let via_static = load_static_artifact(&cli.static_path)?;
    let static_revision = via_static.revisions.transit_static.clone();
    let schedule_revision = via_static.revisions.schedule.clone();
    let mut limits = RoutingLimits::default();
    if let Some(max_departures) = cli.max_departures_per_query {
        limits.max_departures_per_query = max_departures;
    }
    if let Some(max_static_corridors) = cli.max_static_corridors {
        limits.max_static_corridors = max_static_corridors.max(1);
    }
    let valhalla_timeout = cli.valhalla_timeout_ms.map(Duration::from_millis);
    if let Some(timeout) = valhalla_timeout {
        limits.request_timeout = timeout.saturating_mul(5);
    }
    let max_static_corridors = limits.max_static_corridors;
    let options = ProductionRuntimeOptions {
        limits,
        valhalla_request_timeout: valhalla_timeout,
    };
    let (_, runtime) = build_production_runtime(via_static, options).await?;
    let request = request_from_cli(&cli)?;
    if cli.trace {
        eprintln!("  max static corridors={max_static_corridors}");
        eprintln!("ACCESS");
        eprintln!("  origin ({:.6}, {:.6})", cli.from_lat, cli.from_lon);
        eprintln!("  destination ({:.6}, {:.6})", cli.to_lat, cli.to_lon);
        eprintln!("  modes={}", cli.modes);
        eprintln!("  static artifact={}", cli.static_path.display());
        eprintln!("  transit revision={static_revision}");
    }
    let result = runtime
        .routing
        .route(request, new_request_id())
        .await
        .map_err(|error| format!("routing failed: {error:?}"))?;
    if cli.trace {
        eprintln!("{}", runtime.bus_timing_metrics.trace());
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
        locale: "en-US".into(),
        departure_setting,
    };
    request
        .validate()
        .map_err(|error| Box::new(error) as Box<dyn Error>)?;
    Ok(request)
}

fn parse_modes(value: &str) -> Result<Vec<PlanningMode>, Box<dyn Error>> {
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

fn print_summary(result: &PlanningOutcome, network_revision: &str, schedule_revision: &str) {
    println!("network revision: {network_revision}");
    println!("schedule revision: {schedule_revision}");
    println!("service-condition mode: live");
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

#[cfg(test)]
mod tests {
    use super::parse_modes;
    use via_server::api::PlanningMode;

    #[test]
    fn accepts_bike_share_with_transit_modes() {
        let modes = parse_modes("bus,metro,bike_share").unwrap();
        assert_eq!(modes.len(), 3);
        assert!(modes.contains(&PlanningMode::BikeShare));
    }
}
