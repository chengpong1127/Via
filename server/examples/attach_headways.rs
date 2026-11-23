use std::path::PathBuf;

use time::macros::date;
use via_server::data::{bus_headways, load_static_artifact};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut artifact = load_static_artifact(&PathBuf::from(&args[1])).expect("artifact");
    let mut schedules = Vec::new();
    for entry in std::fs::read_dir(&args[2]).unwrap() {
        let path = entry.unwrap().path();
        let body = std::fs::read_to_string(&path).unwrap();
        let records: Vec<bus_headways::BusSchedule> = serde_json::from_str(&body).unwrap();
        eprintln!("{} {}", path.display(), records.len());
        schedules.extend(records);
    }
    let (headways, diagnostics) = bus_headways::pattern_headways(
        &artifact.transit,
        &schedules,
        date!(2026 - 11 - 25),
        date!(2026 - 11 - 28),
    );
    eprintln!(
        "gtfs={} tdx={} without={} unmapped={}",
        diagnostics.from_gtfs,
        diagnostics.from_tdx,
        diagnostics.without_headways,
        diagnostics.unmapped_tdx_sub_routes.len()
    );
    let estimates = bus_headways::attach_estimate_headways(
        &mut artifact.provider_bindings,
        &artifact.transit,
        &headways,
    );
    let parents = artifact
        .provider_bindings
        .bus_patterns
        .values()
        .filter(|binding| binding.tdx_route_uid.is_some())
        .count();
    eprintln!("bus bindings with estimated departures: {estimates}; with parent route: {parents}");
    std::fs::write(&args[3], serde_json::to_vec(&artifact).unwrap()).unwrap();
}
