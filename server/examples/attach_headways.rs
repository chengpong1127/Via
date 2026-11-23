use std::path::PathBuf;

use time::macros::date;
use via_server::data::{bus_headways, bus_routes, load_static_artifact};

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
    if let Some(dir) = args.get(4) {
        let mut routes = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let is_route_file = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("bus-routes-"));
            if is_route_file {
                let records: Vec<bus_routes::BusRoute> =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                routes.extend(records);
            }
        }
        let attached = bus_routes::attach_parent_routes(&mut artifact.provider_bindings, &routes);
        eprintln!(
            "bus routes {} -> parent bindings attached {attached}",
            routes.len()
        );
    }
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
