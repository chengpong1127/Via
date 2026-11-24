// Question (EXP-009): how many nearby places lose their walking transfer to the
// max_outgoing = 12 fan-out cap? Run: fanout_audit <artifact> [crow_fly_metres]
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use via_server::{
    data::load_static_artifact,
    domain::transit::{network::TransitMode, runtime::TransitRuntime},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let artifact = load_static_artifact(&PathBuf::from(&args[1]))?;
    let radius: u32 = args.get(2).map_or(350, |v| v.parse().unwrap());
    let transit = TransitRuntime::new(Arc::new(artifact.transit));
    let dense = transit.dense();
    let stops = &transit.network().stops;

    let mut stop_modes: HashMap<usize, Vec<TransitMode>> = HashMap::new();
    for stop in stops {
        let idx = dense.stop_index(&stop.id).unwrap() as usize;
        let mut modes: Vec<TransitMode> = transit
            .pattern_positions_at(&stop.id)
            .iter()
            .filter_map(|p| transit.pattern(&p.pattern_id))
            .map(|p| transit.required_route(&p.route_id).mode)
            .collect();
        modes.sort_unstable();
        modes.dedup();
        stop_modes.insert(idx, modes);
    }

    let mut used = 0usize;
    let mut capped = 0usize;
    let mut with_missing = 0usize;
    let mut missing_places = 0usize;
    let mut candidate_places = 0usize;
    let mut missing_rail_places = 0usize;
    let mut stops_missing_rail = 0usize;
    let mut out_hist = [0usize; 14];
    let mut dup_targets_in_edges = 0usize;
    let mut total_edges = 0usize;
    let mut places_in_edges = 0usize;
    let mut bus_only_places_missing = 0usize;

    for stop in stops {
        let idx = dense.stop_index(&stop.id).unwrap();
        if stop_modes[&(idx as usize)].is_empty() {
            continue;
        }
        used += 1;
        let edges = transit.transfers_from(&stop.id);
        out_hist[edges.len().min(13)] += 1;
        if edges.len() >= 12 {
            capped += 1;
        }
        let own_place = dense.place(idx);
        let mut reached: HashSet<u32> = HashSet::new();
        reached.insert(own_place);
        let mut edge_places = HashSet::new();
        for edge in edges {
            let to = dense.stop_index(&edge.to).unwrap();
            reached.insert(dense.place(to));
            edge_places.insert(dense.place(to));
        }
        total_edges += edges.len();
        places_in_edges += edge_places.len();
        dup_targets_in_edges += edges.len() - edge_places.len();

        let mut candidates: HashMap<u32, Vec<TransitMode>> = HashMap::new();
        for near in transit.stops_within_radius(stop.coordinate, radius) {
            let nidx = dense.stop_index(&near.id).unwrap();
            let modes = &stop_modes[&(nidx as usize)];
            if modes.is_empty() {
                continue;
            }
            let entry = candidates.entry(dense.place(nidx)).or_default();
            entry.extend(modes.iter().copied());
        }
        let mut missing = 0usize;
        let mut missing_rail = false;
        for (place, modes) in &candidates {
            if place == &own_place {
                continue;
            }
            candidate_places += 1;
            if !reached.contains(place) {
                missing += 1;
                let rail = modes
                    .iter()
                    .any(|m| !matches!(m, TransitMode::Bus));
                if rail {
                    missing_rail_places += 1;
                    missing_rail = true;
                } else {
                    bus_only_places_missing += 1;
                }
            }
        }
        missing_places += missing;
        if missing > 0 {
            with_missing += 1;
        }
        if missing_rail {
            stops_missing_rail += 1;
        }
    }

    println!("crow_fly_radius_m={radius} used_stops={used}");
    println!("outgoing histogram (0..12, 13+): {out_hist:?}");
    println!("stops at the 12 cap: {capped} ({:.1} %)", 100.0 * capped as f64 / used as f64);
    println!(
        "mean edges/stop {:.2}, mean distinct target places/stop {:.2}, edges to an already-reached place {:.1} %",
        total_edges as f64 / used as f64,
        places_in_edges as f64 / used as f64,
        100.0 * dup_targets_in_edges as f64 / total_edges as f64
    );
    println!(
        "candidate nearby places {candidate_places}, with no transfer edge {missing_places} ({:.1} %)",
        100.0 * missing_places as f64 / candidate_places as f64
    );
    println!(
        "stops with >=1 missing place: {with_missing} ({:.1} %); missing places with a non-bus mode: {missing_rail_places}; bus-only missing: {bus_only_places_missing}; stops missing a non-bus place: {stops_missing_rail} ({:.1} %)",
        100.0 * with_missing as f64 / used as f64,
        100.0 * stops_missing_rail as f64 / used as f64
    );
    Ok(())
}
