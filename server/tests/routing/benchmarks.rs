use std::{hint::black_box, sync::Arc, time::Instant};

use time::macros::datetime;
use via_server::{
    domain::{ids::BoardingPointId, transit::TransitMode, transit_runtime::TransitRuntime},
    providers::{
        BusDepartureSource, DepartureDispatcher, MetroDepartureSource, ThsrDepartureSource,
        TraDepartureSource, TransitDepartureSource,
    },
    routing::{
        AccessPoint, EgressPoint, TransitPlanner, TransitQuery, static_search,
        topology::TransitTopologyIndex, transfers::TransferShortcutIndex,
    },
};

use super::fixtures::network;

// Run the same command/profile before and after a refactor; there are no machine-dependent thresholds.
#[tokio::test]
#[ignore = "in-process routing performance comparison"]
async fn deterministic_routing_benchmark() {
    let runtime = Arc::new(TransitRuntime::new(Arc::new(network())));
    let topology = TransitTopologyIndex::new(runtime.data());
    let transfers = TransferShortcutIndex::from_network(runtime.data());
    let sources: Vec<Arc<dyn TransitDepartureSource>> = vec![
        Arc::new(BusDepartureSource::new(runtime.clone(), None)),
        Arc::new(MetroDepartureSource::new(runtime.clone())),
        Arc::new(TraDepartureSource::new(runtime.clone())),
        Arc::new(ThsrDepartureSource::new(runtime.clone())),
    ];
    let planner = TransitPlanner::new(
        runtime.clone(),
        Arc::new(DepartureDispatcher::new(runtime.clone(), sources)),
    );
    let cases = [
        ("metro", "F-M1", "F-M2-RED", vec![TransitMode::Metro]),
        ("bus", "F-BUS-A", "F-BUS-B", vec![TransitMode::Bus]),
        (
            "bus_metro",
            "F-BUS-A",
            "F-M3-BLUE",
            vec![TransitMode::Bus, TransitMode::Metro],
        ),
        (
            "metro_bus",
            "F-M1",
            "F-BUS-B",
            vec![TransitMode::Bus, TransitMode::Metro],
        ),
        (
            "multiple_transfers",
            "F-M1",
            "F-M4",
            vec![TransitMode::Metro],
        ),
    ];
    let departure = datetime!(2026-08-31 10:00 +08:00);
    const WARMUP: usize = 20;
    const ITERATIONS: usize = 1000;
    for (label, origin, destination, modes) in cases {
        let query = TransitQuery::new(
            vec![AccessPoint::new(
                BoardingPointId::new(origin),
                departure,
                departure,
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new(destination), 0, 0)],
            departure,
        )
        .with_allowed_transit_modes(modes);
        let mut static_samples = Vec::with_capacity(ITERATIONS);
        let mut planner_samples = Vec::with_capacity(ITERATIONS);
        let mut expected = None;
        for iteration in 0..WARMUP + ITERATIONS {
            let started = Instant::now();
            let (corridors, static_stats) =
                static_search::generate(&runtime, &topology, &transfers, black_box(&query));
            let static_ns = started.elapsed().as_nanos();
            let started = Instant::now();
            let (variants, planner_stats) = planner
                .plan_with_stats(black_box(query.clone()))
                .await
                .unwrap();
            let planner_ns = started.elapsed().as_nanos();
            let counters = (
                corridors.retained().len(),
                static_stats.static_search_states_expanded,
                variants.len(),
                planner_stats.phase2_evaluation.transit_departure_lookups,
            );
            if let Some(expected) = expected {
                assert_eq!(counters, expected);
            } else {
                expected = Some(counters);
            }
            if iteration >= WARMUP {
                static_samples.push(static_ns);
                planner_samples.push(planner_ns);
            }
        }
        static_samples.sort_unstable();
        planner_samples.sort_unstable();
        let (corridors, expansions, variants, lookups) = expected.unwrap();
        assert!(
            corridors > 0,
            "benchmark must exercise retained corridors: {label}"
        );
        assert!(variants > 0, "benchmark must realize journeys: {label}");
        println!(
            "{label}: iterations={ITERATIONS} static_median_us={:.3} planner_median_us={:.3} corridors={corridors} expansions={expansions} variants={variants} departure_lookups={lookups}",
            static_samples[ITERATIONS / 2] as f64 / 1000.0,
            planner_samples[ITERATIONS / 2] as f64 / 1000.0
        );
    }
}
