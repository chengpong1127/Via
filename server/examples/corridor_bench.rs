use std::{
    collections::HashSet,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use time::macros::datetime;
use via_server::{
    config::ViaConfig,
    data::load_static_artifact,
    domain::{
        ids::BoardingPointId,
        location::GeoCoordinate,
        transit::{network::TransitMode, runtime::TransitRuntime},
    },
    routing::{EndpointStop, PlanningDeadline, PlanningQuery, generate_static_corridors},
    street::Coordinate,
};

type RideGroup = (
    BoardingPointId,
    BoardingPointId,
    Vec<String>,
    u32,
    TransitMode,
    Vec<String>,
);

const WALK_SPEED_MPS: f64 = 1.25;
const DETOUR_FACTOR: f64 = 1.3;
const MAX_WALK_SECONDS: u32 = 1800;
const RADIUS_METRES: u32 = 2000;
const ENDPOINT_LIMIT: usize = 64;

fn haversine(a: GeoCoordinate, b: GeoCoordinate) -> f64 {
    let r = 6_371_000.0_f64;
    let (la1, la2) = (a.latitude.to_radians(), b.latitude.to_radians());
    let dla = la2 - la1;
    let dlo = (b.longitude - a.longitude).to_radians();
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}

fn modes_at(transit: &TransitRuntime, stop: &BoardingPointId) -> Vec<TransitMode> {
    let mut modes: Vec<_> = transit
        .pattern_positions_at(stop)
        .iter()
        .filter_map(|p| transit.pattern(&p.pattern_id))
        .map(|p| transit.required_route(&p.route_id).mode)
        .collect();
    modes.sort_unstable();
    modes.dedup();
    modes
}

fn endpoints(
    transit: &TransitRuntime,
    at: GeoCoordinate,
    modes: &[TransitMode],
) -> Vec<EndpointStop> {
    let mut walks: Vec<EndpointStop> = transit
        .stops_within_radius(at, RADIUS_METRES)
        .into_iter()
        .filter(|stop| {
            modes_at(transit, &stop.id)
                .iter()
                .any(|m| modes.contains(m))
        })
        .filter_map(|stop| {
            let metres = haversine(at, stop.coordinate) * DETOUR_FACTOR;
            let seconds = (metres / WALK_SPEED_MPS).round() as u32;
            (seconds <= MAX_WALK_SECONDS)
                .then(|| EndpointStop::new(stop.id.clone(), seconds, metres as u32))
        })
        .collect();
    walks.sort_by(|l, r| {
        l.walking_seconds
            .cmp(&r.walking_seconds)
            .then(l.stop_id.as_str().cmp(r.stop_id.as_str()))
    });
    let mut kept: Vec<EndpointStop> = Vec::new();
    for stop in walks {
        let patterns: HashSet<_> = transit
            .pattern_positions_at(&stop.stop_id)
            .iter()
            .map(|p| p.pattern_id.clone())
            .collect();
        let dominated = kept.iter().any(|known| {
            let known_patterns: HashSet<_> = transit
                .pattern_positions_at(&known.stop_id)
                .iter()
                .map(|p| p.pattern_id.clone())
                .collect();
            patterns.is_subset(&known_patterns)
        });
        if !dominated {
            kept.push(stop);
        }
    }
    kept.truncate(ENDPOINT_LIMIT);
    kept
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

fn scenarios() -> Vec<(String, GeoCoordinate, GeoCoordinate)> {
    let named = [
        (
            "taipei_main_to_xinyi",
            (25.0478, 121.5170),
            (25.0330, 121.5654),
        ),
        ("ntu_to_nangang", (25.0174, 121.5397), (25.0553, 121.6171)),
        ("banqiao_to_neihu", (25.0143, 121.4672), (25.0798, 121.5753)),
        (
            "tamsui_to_xindian",
            (25.1679, 121.4455),
            (24.9576, 121.5375),
        ),
        (
            "shilin_to_zhonghe",
            (25.0930, 121.5260),
            (24.9990, 121.5090),
        ),
        (
            "songshan_airport_to_wanhua",
            (25.0634, 121.5523),
            (25.0360, 121.5000),
        ),
        (
            "xinzhuang_to_muzha",
            (25.0362, 121.4517),
            (24.9986, 121.5732),
        ),
        (
            "sanchong_to_ximen",
            (25.0617, 121.4870),
            (25.0421, 121.5081),
        ),
        (
            "taoyuan_to_taipei",
            (24.9896, 121.3133),
            (25.0478, 121.5170),
        ),
        (
            "keelung_to_taipei",
            (25.1316, 121.7400),
            (25.0478, 121.5170),
        ),
    ];
    let mut out: Vec<_> = named
        .iter()
        .map(|(n, a, b)| {
            (
                n.to_string(),
                GeoCoordinate {
                    latitude: a.0,
                    longitude: a.1,
                },
                GeoCoordinate {
                    latitude: b.0,
                    longitude: b.1,
                },
            )
        })
        .collect();
    let mut rng = Lcg(20261121);
    let mut index = 0;
    while out.len() < named.len() + 90 {
        let a = GeoCoordinate {
            latitude: 24.97 + rng.next() * 0.17,
            longitude: 121.43 + rng.next() * 0.22,
        };
        let b = GeoCoordinate {
            latitude: 24.97 + rng.next() * 0.17,
            longitude: 121.43 + rng.next() * 0.22,
        };
        let km = haversine(a, b) / 1000.0;
        if !(2.5..=18.0).contains(&km) {
            continue;
        }
        out.push((format!("random_{index:03}"), a, b));
        index += 1;
    }
    out
}

// EXP-009 prototype: crow-fly (x1.3 detour, 1.4 m/s) transfer edges to nearby places that the
// 12-edge fan-out cap left out. Upper bound: real walkability is not checked.
fn place_fanout_edges(
    transit: &TransitRuntime,
    max_places: usize,
) -> Vec<via_server::domain::transit::network::TransferEdge> {
    let dense = transit.dense();
    let mut added = Vec::new();
    for stop in &transit.network().stops {
        if transit.pattern_positions_at(&stop.id).is_empty() {
            continue;
        }
        let idx = dense.stop_index(&stop.id).unwrap();
        let mut reached: HashSet<u32> = HashSet::new();
        reached.insert(dense.place(idx));
        for edge in transit.transfers_from(&stop.id) {
            reached.insert(dense.place(dense.stop_index(&edge.to).unwrap()));
        }
        if reached.len() > max_places {
            continue;
        }
        let mut best: std::collections::HashMap<u32, (f64, &BoardingPointId)> =
            std::collections::HashMap::new();
        for near in transit.stops_within_radius(stop.coordinate, 400) {
            if transit.pattern_positions_at(&near.id).is_empty() {
                continue;
            }
            let place = dense.place(dense.stop_index(&near.id).unwrap());
            if reached.contains(&place) {
                continue;
            }
            let metres = haversine(stop.coordinate, near.coordinate) * DETOUR_FACTOR;
            if metres > 600.0 {
                continue;
            }
            let entry = best.entry(place).or_insert((metres, &near.id));
            if metres < entry.0 {
                *entry = (metres, &near.id);
            }
        }
        let mut ranked: Vec<_> = best.into_values().collect();
        ranked.sort_by(|l, r| l.0.partial_cmp(&r.0).unwrap().then(l.1.as_str().cmp(r.1.as_str())));
        for (metres, target) in ranked.into_iter().take(max_places - reached.len() + 1) {
            added.push(via_server::domain::transit::network::TransferEdge::new(
                stop.id.clone(),
                target.clone(),
                (metres / 1.4).ceil() as u32,
                metres as u32,
            ));
        }
    }
    added
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let static_path = PathBuf::from(
        args.get(1)
            .expect("usage: corridor_bench <static> <out.jsonl> [iterations]"),
    );
    let out_path = PathBuf::from(args.get(2).expect("output path"));
    let iterations: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(5);
    via_server::domain::transit::topology::PLACE_EQUIVALENCE.store(
        std::env::var("VIA_PLACE").map_or(true, |value| value != "0"),
        std::sync::atomic::Ordering::SeqCst,
    );
    let loaded = Instant::now();
    let via_static = load_static_artifact(&static_path).expect("artifact");
    let mut transit = via_static.transit;
    if let Ok(extra) = std::env::var("VIA_FANOUT_PLACES") {
        let extra: usize = extra.parse().unwrap();
        let network = Arc::new(transit);
        let probe = TransitRuntime::new(Arc::clone(&network));
        let added = place_fanout_edges(&probe, extra);
        drop(probe);
        transit = Arc::try_unwrap(network).ok().expect("unique network");
        eprintln!("added {} place fan-out edges", added.len());
        transit.transfer_edges.extend(added);
        transit = transit.canonicalize();
    }
    if let Ok(path) = std::env::var("VIA_TRANSFER_EDGES") {
        transit.transfer_edges = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        transit = transit.canonicalize();
        eprintln!("replaced transfer edges: {}", transit.transfer_edges.len());
    }
    let runtime = TransitRuntime::new(Arc::new(transit));
    eprintln!("loaded artifact in {:?}", loaded.elapsed());
    let limits = ViaConfig::default().planner_limits();
    let modes = vec![
        TransitMode::Bus,
        TransitMode::Metro,
        TransitMode::Rail,
        TransitMode::HighSpeedRail,
    ];
    let departure = args
        .get(4)
        .map(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                .unwrap()
        })
        .unwrap_or(datetime!(2026-11-23 08:30 +08:00));
    let mut out = std::fs::File::create(&out_path).unwrap();
    let mut all_ms = Vec::new();
    for (name, from, to) in scenarios() {
        let access = endpoints(&runtime, from, &modes);
        let egress = endpoints(&runtime, to, &modes);
        let query = PlanningQuery::new(
            Coordinate::try_new(from.latitude, from.longitude).unwrap(),
            Coordinate::try_new(to.latitude, to.longitude).unwrap(),
            PlanningDeadline::after(Duration::from_secs(600)),
            access.clone(),
            egress.clone(),
            departure,
            limits,
        )
        .with_allowed_transit_modes(modes.clone());
        let mut samples = Vec::new();
        let mut result = None;
        for _ in 0..iterations {
            let started = Instant::now();
            let r = generate_static_corridors(&runtime, std::hint::black_box(&query));
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            result = Some(r);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = samples[samples.len() / 2];
        all_ms.push(median);
        let (corridors, stats) = match result.unwrap() {
            Ok(v) => v,
            Err(e) => {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({"name": name, "error": e.to_string()})
                )
                .unwrap();
                continue;
            }
        };
        let stop_name = |id: &BoardingPointId| {
            runtime
                .stop(id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| id.as_str().to_owned())
        };
        let described: Vec<_> = corridors
            .iter()
            .map(|c| {
                let mut groups: Vec<RideGroup> = Vec::new();
                for leg in c.transit_legs() {
                    let pattern = runtime.pattern(&leg.pattern_id).unwrap();
                    let route = runtime.required_route(&pattern.route_id);
                    let secs: u32 = pattern.static_profile.segments
                        [leg.board_index..leg.alight_index]
                        .iter()
                        .map(|s| s.seconds)
                        .sum();
                    match groups.last_mut() {
                        Some(g) if g.0 == leg.board_stop_id && g.1 == leg.alight_stop_id => {
                            if !g.2.contains(&route.label) {
                                g.2.push(route.label.clone());
                            }
                            g.3 = g.3.min(secs);
                            g.5.push(leg.pattern_id.as_str().to_owned());
                        }
                        _ => groups.push((
                            leg.board_stop_id.clone(),
                            leg.alight_stop_id.clone(),
                            vec![route.label.clone()],
                            secs,
                            route.mode,
                            vec![leg.pattern_id.as_str().to_owned()],
                        )),
                    }
                }
                let anchors: Vec<String> = c
                    .anchors()
                    .iter()
                    .filter_map(|a| a.boarding_point().map(|id| id.as_str().to_owned()))
                    .collect();
                let rides: Vec<String> = groups
                    .iter()
                    .map(|g| {
                        format!(
                            "{:?}[{}] {}>{} {}s",
                            g.4,
                            g.2.join("/"),
                            stop_name(&g.0),
                            stop_name(&g.1),
                            g.3
                        )
                    })
                    .collect();
                let stops: Vec<BoardingPointId> = c
                    .anchors()
                    .iter()
                    .filter_map(|a| a.boarding_point().cloned())
                    .collect();
                let mut seconds = access
                    .iter()
                    .find(|e| Some(&e.stop_id) == stops.first())
                    .map_or(0, |e| e.walking_seconds)
                    + egress
                        .iter()
                        .find(|e| Some(&e.stop_id) == stops.last())
                        .map_or(0, |e| e.walking_seconds);
                for pair in stops.windows(2) {
                    if let Some(g) = groups.iter().find(|g| g.0 == pair[0] && g.1 == pair[1]) {
                        seconds += g.3;
                    } else {
                        seconds += runtime
                            .transfers_from(&pair[0])
                            .iter()
                            .filter(|t| t.to == pair[1])
                            .map(|t| t.duration_seconds)
                            .min()
                            .unwrap_or(0);
                    }
                }
                let ride_patterns: Vec<_> = groups.iter().map(|g| g.5.clone()).collect();
                let edge_sets: Vec<_> = c
                    .edges()
                    .iter()
                    .filter_map(|edge| match edge {
                        via_server::routing::PlannedMovement::Transit { legs } => {
                            let mut routes = legs
                                .iter()
                                .map(|leg| {
                                    let pattern = runtime.pattern(&leg.pattern_id).unwrap();
                                    runtime.required_route(&pattern.route_id).label.clone()
                                })
                                .collect::<Vec<_>>();
                            routes.sort();
                            routes.dedup();
                            let mut boards = legs
                                .iter()
                                .map(|leg| leg.board_stop_id.as_str().to_owned())
                                .collect::<Vec<_>>();
                            boards.sort();
                            boards.dedup();
                            Some(serde_json::json!({"legs": legs.len(), "routes": routes, "board_stops": boards}))
                        }
                        _ => None,
                    })
                    .collect();
                serde_json::json!({"edge_sets": edge_sets, "ride_patterns": ride_patterns, "rides": rides, "anchors": anchors, "static_seconds": seconds, "transfers": groups.len().saturating_sub(1)})
            })
            .collect();
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "name": name,
                "from": [from.latitude, from.longitude],
                "to": [to.latitude, to.longitude],
                "access": access.len(),
                "egress": egress.len(),
                "median_ms": median,
                "states_expanded": stats.states_expanded,
                "unique_corridors_found": stats.unique_corridors_found,
                "corridors": described,
            })
        )
        .unwrap();
    }
    all_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| all_ms[((all_ms.len() as f64 - 1.0) * p).round() as usize];
    println!(
        "queries={} p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} max_ms={:.2} mean_ms={:.2}",
        all_ms.len(),
        pct(0.5),
        pct(0.95),
        pct(0.99),
        all_ms.last().unwrap(),
        all_ms.iter().sum::<f64>() / all_ms.len() as f64
    );
}
