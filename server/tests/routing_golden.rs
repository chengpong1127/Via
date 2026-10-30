use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use time::OffsetDateTime;
use via_server::{
    api::PlanningRequest,
    domain::{
        bikeshare::{
            BikeAvailabilityError, BikeAvailabilitySnapshot, BikeAvailabilitySource,
            BikeStationAvailability,
        },
        ids::BikeStationId,
        location::GeoCoordinate,
        planning::PlanningInput,
        transit_runtime::TransitRuntime,
        transit_timing::{
            DepartureQuery, ScheduleTiming, SnapshotDepartures, TimingReliability, TimingSnapshot,
            TimingSnapshotLoader,
        },
    },
    routing::RoutingService,
    street::StraightLineStreetRouter,
};

struct ScheduleSnapshots(ScheduleTiming);

#[async_trait::async_trait]
impl TimingSnapshotLoader for ScheduleSnapshots {
    async fn load_snapshot(&self, queries: Vec<DepartureQuery>) -> TimingSnapshot {
        let mut snapshot = TimingSnapshot::default();
        for query in queries {
            let mut trips = self
                .0
                .next_departures(query.clone())
                .await
                .unwrap_or_default();
            trips.extend(
                self.0
                    .next_frequency_departures(query.clone())
                    .unwrap_or_default(),
            );
            for trip in &mut trips {
                trip.timing_reliability = TimingReliability::Fallback;
            }
            snapshot.insert_entry(
                query.pattern_id,
                query.board_index.unwrap(),
                SnapshotDepartures {
                    fallback: trips,
                    ..Default::default()
                },
            );
        }
        snapshot
    }
}

struct AvailableBikes;

#[async_trait::async_trait]
impl BikeAvailabilitySource for AvailableBikes {
    async fn snapshot(
        &self,
        ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        let now = OffsetDateTime::now_utc();
        let stations = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let bikes = if index % 7 == 3 { 0 } else { 5 };
                let docks = if index % 11 == 5 { 0 } else { 5 };
                (
                    id.clone(),
                    BikeStationAvailability::new(id.clone(), bikes, docks, true, now),
                )
            })
            .collect();
        Ok(Arc::new(BikeAvailabilitySnapshot::new(now, stations)))
    }
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % bound as u64) as usize
    }
}

fn od_pairs(runtime: &TransitRuntime, per_bucket: usize) -> Vec<(GeoCoordinate, GeoCoordinate)> {
    let mut stops = runtime
        .network()
        .stops
        .iter()
        .filter(|stop| {
            (24.95..25.20).contains(&stop.coordinate.latitude)
                && (121.40..121.65).contains(&stop.coordinate.longitude)
        })
        .collect::<Vec<_>>();
    stops.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut random = Lcg(0x5eed);
    let mut pairs = Vec::new();
    for (minimum, maximum) in [(300.0, 1_500.0), (1_500.0, 5_000.0), (5_000.0, 15_000.0)] {
        let mut found = 0;
        while found < per_bucket {
            let origin = stops[random.next(stops.len())].coordinate;
            let destination = stops[random.next(stops.len())].coordinate;
            let distance = origin.distance_metres(destination);
            if distance < minimum || distance >= maximum {
                continue;
            }
            let shift = |coordinate: GeoCoordinate, latitude: f64, longitude: f64| GeoCoordinate {
                latitude: coordinate.latitude + latitude,
                longitude: coordinate.longitude + longitude,
            };
            pairs.push((
                shift(origin, 0.0006, -0.0004),
                shift(destination, -0.0005, 0.0007),
            ));
            found += 1;
        }
    }
    pairs
}

fn request(
    origin: GeoCoordinate,
    destination: GeoCoordinate,
    modes: &[&str],
    departure: &str,
) -> PlanningInput {
    serde_json::from_value::<PlanningRequest>(serde_json::json!({
        "origin": {"coordinate": {"latitude": origin.latitude, "longitude": origin.longitude}, "display_text": "Origin"},
        "destination": {"coordinate": {"latitude": destination.latitude, "longitude": destination.longitude}, "display_text": "Destination"},
        "selected_modes": modes,
        "departure_setting": {"type": "depart_at", "time": departure},
    }))
    .unwrap()
    .into()
}

fn without_generated_at(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("generated_at");
    }
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a local static artifact; writes a golden routing transcript"]
async fn routing_golden_transcript() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let artifact_path = std::env::var_os("VIA_GOLDEN_ARTIFACT")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("data/build/.via-static-latest.json"));
    let output_path = std::env::var_os("VIA_GOLDEN_OUTPUT")
        .map(PathBuf::from)
        .expect("VIA_GOLDEN_OUTPUT must name the transcript file");
    let per_bucket = std::env::var("VIA_GOLDEN_PAIRS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12);
    let artifact = via_server::data::load_static_artifact(Path::new(&artifact_path)).unwrap();
    let runtime = Arc::new(TransitRuntime::new(Arc::new(artifact.transit)));
    let bike_network = Arc::new(artifact.bike_share);
    let config = via_server::config::ViaConfig::load(Some(&root.join("config/via.toml"))).unwrap();
    let default_limits = config.journey_limits();
    let mut wide_limits = default_limits.clone();
    wide_limits.ranking.result_limit = wide_limits.planner.max_candidates;
    let services = [("default", default_limits), ("wide", wide_limits)].map(|(label, limits)| {
        (
            label,
            RoutingService::new(
                Arc::new(StraightLineStreetRouter::default()),
                runtime.clone(),
                Arc::new(ScheduleSnapshots(ScheduleTiming::new(runtime.clone()))),
                bike_network.clone(),
                Arc::new(AvailableBikes),
                limits,
                false,
            ),
        )
    });
    let mode_sets: [&[&str]; 5] = [
        &["bus", "metro"],
        &["bus", "metro", "bike_share"],
        &["metro", "tra", "thsr"],
        &["bike_share"],
        &[],
    ];
    let departures = ["2026-10-01T08:10:00+08:00", "2026-10-01T22:45:00+08:00"];
    let mut output = std::io::BufWriter::new(std::fs::File::create(output_path).unwrap());
    let started = Instant::now();
    let mut cases = 0;
    for (pair_index, (origin, destination)) in
        od_pairs(&runtime, per_bucket).into_iter().enumerate()
    {
        for modes in mode_sets {
            for departure in departures {
                for (label, service) in &services {
                    let result = service
                        .plan(request(origin, destination, modes, departure))
                        .await;
                    let result = match result {
                        Ok(outcome) => without_generated_at(serde_json::to_value(outcome).unwrap()),
                        Err(error) => serde_json::json!({"error": format!("{error:?}")}),
                    };
                    let line = serde_json::json!({
                        "pair": pair_index,
                        "modes": modes,
                        "departure": departure,
                        "limits": label,
                        "result": result,
                    });
                    writeln!(output, "{line}").unwrap();
                    cases += 1;
                }
            }
        }
    }
    output.flush().unwrap();
    println!(
        "golden cases={cases} elapsed_ms={}",
        started.elapsed().as_millis()
    );
}
