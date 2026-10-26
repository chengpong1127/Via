use std::{hint::black_box, sync::Arc, time::Instant};

use time::macros::datetime;
use via_server::{
    domain::{
        bikeshare::BikeShareNetwork, ids::BoardingPointId, transit::TransitMode,
        transit_runtime::TransitRuntime,
    },
    providers::{
        BusDepartureSource, DepartureResolver, MetroDepartureSource, ThsrDepartureSource,
        TraDepartureSource, TransitDepartureSource, UnavailableBikeAvailabilitySource,
    },
    routing::{
        AccessPoint, EgressPoint, MultimodalPlanner, PlanningDeadline, PlanningQuery,
        static_search, topology::TransitTopologyIndex, transfers::TransferShortcutIndex,
    },
};

use super::fixtures::network;
use via_server::street::FakeStreetRouter;

// Run the same command/profile before and after a refactor; there are no machine-dependent thresholds.
#[tokio::test]
#[ignore = "in-process routing performance comparison"]
async fn deterministic_routing_benchmark() {
    let runtime = Arc::new(TransitRuntime::new(Arc::new(network())));
    let topology = TransitTopologyIndex::new(runtime.network());
    let transfers = TransferShortcutIndex::from_network(runtime.network());
    let sources: Vec<Arc<dyn TransitDepartureSource>> = vec![
        Arc::new(BusDepartureSource::new(
            runtime.clone(),
            crate::support::tdx::failing_tdx_client(),
        )),
        Arc::new(MetroDepartureSource::new(
            runtime.clone(),
            crate::support::tdx::failing_tdx_client(),
        )),
        Arc::new(TraDepartureSource::new(
            runtime.clone(),
            crate::support::tdx::failing_tdx_client(),
        )),
        Arc::new(ThsrDepartureSource::new(runtime.clone())),
    ];
    let planner = MultimodalPlanner::new(
        runtime.clone(),
        Arc::new(DepartureResolver::new(runtime.clone(), sources)),
        Arc::new(FakeStreetRouter::builder().build()),
        Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("benchmark-disabled")),
        Arc::new(UnavailableBikeAvailabilitySource),
        via_server::config::ViaConfig::default().bike_policy(),
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
        let query = PlanningQuery::new(
            runtime
                .stop(&BoardingPointId::new(origin))
                .unwrap()
                .coordinate
                .try_into()
                .unwrap(),
            runtime
                .stop(&BoardingPointId::new(destination))
                .unwrap()
                .coordinate
                .try_into()
                .unwrap(),
            PlanningDeadline::after(std::time::Duration::from_secs(60)),
            vec![AccessPoint::new(
                BoardingPointId::new(origin),
                departure,
                departure,
                0,
                0,
            )],
            vec![EgressPoint::new(BoardingPointId::new(destination), 0, 0)],
            departure,
            via_server::config::ViaConfig::default().planner_limits(),
        )
        .with_allowed_transit_modes(modes);
        let mut static_samples = Vec::with_capacity(ITERATIONS);
        let mut planner_samples = Vec::with_capacity(ITERATIONS);
        let mut expected = None;
        let mut last_phase2 = None;
        for iteration in 0..WARMUP + ITERATIONS {
            let started = Instant::now();
            let (corridors, static_stats) =
                static_search::generate(&runtime, &topology, &transfers, black_box(&query));
            let static_ns = started.elapsed().as_nanos();
            let started = Instant::now();
            let (realized_journeys, planner_stats) = planner
                .plan_with_stats(black_box(query.clone()))
                .await
                .unwrap();
            let planner_ns = started.elapsed().as_nanos();
            last_phase2 = Some(planner_stats.phase2_evaluation);
            let counters = (
                corridors.retained().len(),
                static_stats.states_expanded,
                realized_journeys.len(),
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
        let (corridors, expansions, realized_journeys, lookups) = expected.unwrap();
        assert!(
            corridors > 0,
            "benchmark must exercise retained corridors: {label}"
        );
        assert!(
            realized_journeys > 0,
            "benchmark must realize journeys: {label}"
        );
        let last_phase2 = last_phase2.unwrap();
        println!(
            "{label}: p2_expansions={} walking_matrix_requests={} walking_cache_hits={} bike_requests={} bike_cache_hits={} bike_matrices={}",
            last_phase2.corridor_states_evaluated,
            last_phase2.walking_matrix_requests,
            last_phase2.walking_cache_hits,
            last_phase2.bike_exact_candidates_requested,
            last_phase2.bike_exact_cache_hits,
            last_phase2.bike_matrix_requests
        );
        println!(
            "{label}: iterations={ITERATIONS} static_median_us={:.3} planner_median_us={:.3} corridors={corridors} expansions={expansions} realized_journeys={realized_journeys} departure_lookups={lookups}",
            static_samples[ITERATIONS / 2] as f64 / 1000.0,
            planner_samples[ITERATIONS / 2] as f64 / 1000.0
        );
    }
}
