use std::{collections::HashMap, io::Write, path::PathBuf};

use via_server::data::load_static_artifact;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let artifact = load_static_artifact(&PathBuf::from(&args[1])).expect("artifact");
    let network = artifact.transit;
    let routes: HashMap<_, _> = network.routes.iter().map(|r| (&r.id, r)).collect();
    let names: HashMap<_, _> = network
        .stops
        .iter()
        .map(|s| (&s.id, s.name.as_str()))
        .collect();
    let calendars: HashMap<_, _> = network.calendars.iter().map(|c| (&c.id, c)).collect();
    let day = time::macros::date!(2026 - 11 - 23);
    let mut trips = HashMap::<_, Vec<u32>>::new();
    for trip in &network.trips {
        if !calendars
            .get(&trip.calendar_id)
            .is_some_and(|c| c.runs_on(day))
        {
            continue;
        }
        if let Some(first) = trip.stop_times.first() {
            trips
                .entry(&trip.pattern_id)
                .or_default()
                .push(first.departure_seconds);
        }
    }
    let mut out = std::fs::File::create(&args[2]).unwrap();
    for pattern in &network.patterns {
        let route = routes[&pattern.route_id];
        let departures = trips.get(&pattern.id).cloned().unwrap_or_default();
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "pattern": pattern.id.as_str(),
                "route": pattern.route_id.as_str(),
                "label": route.label,
                "mode": format!("{:?}", route.mode),
                "direction": pattern.direction,
                "stops": pattern.stops.len(),
                "first": pattern.stops.first().map(|s| names.get(s).copied().unwrap_or("")),
                "last": pattern.stops.last().map(|s| names.get(s).copied().unwrap_or("")),
                "trip_departures": departures,
            })
        )
        .unwrap();
    }
}
