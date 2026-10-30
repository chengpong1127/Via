use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use time::{Time, Weekday, macros::datetime};

use super::*;
use crate::{
    domain::location::GeoCoordinate,
    domain::{
        bikeshare::{BikeAvailabilitySnapshot, BikeShareNetwork},
        ids::{BoardingPointId, EffectiveTripId, TransitPatternId, TransitTripId},
        transit::{
            ScheduledStopTime, TransitMode, TransitNetwork, TransitPattern, TransitStop,
            TransitTrip,
        },
        transit_runtime::TransitRuntime,
        transit_timing::{
            EffectiveStopTime, EffectiveTrip, TimingBasis, TimingReliability, TimingSnapshot,
        },
    },
    routing::{AccessPoint, EgressPoint},
    street::FakeStreetRouter,
};

#[derive(Default)]
struct RecordingTimingLoader {
    queries: Mutex<Vec<DepartureQuery>>,
    calls: AtomicUsize,
    trips: HashMap<TransitPatternId, Vec<EffectiveTrip>>,
}

#[async_trait::async_trait]
impl TimingSnapshotLoader for RecordingTimingLoader {
    async fn load_snapshot(&self, queries: Vec<DepartureQuery>) -> TimingSnapshot {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.queries.lock().unwrap() = queries.clone();
        let mut snapshot = TimingSnapshot::default();
        for query in queries {
            snapshot.insert(
                query.pattern_id.clone(),
                query.board_index.unwrap(),
                self.trips
                    .get(&query.pattern_id)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        snapshot
    }
}

struct CountingBikeAvailabilitySource {
    calls: Arc<AtomicUsize>,
    requested: Arc<Mutex<Vec<BikeStationId>>>,
}

#[async_trait::async_trait]
impl BikeAvailabilitySource for CountingBikeAvailabilitySource {
    async fn snapshot(
        &self,
        station_ids: &[BikeStationId],
    ) -> Result<Arc<BikeAvailabilitySnapshot>, BikeAvailabilityError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.requested.lock().unwrap() = station_ids.to_vec();
        Ok(Arc::new(BikeAvailabilitySnapshot::new(
            datetime!(2026-09-21 10:00 UTC),
            HashMap::new(),
        )))
    }
}

fn stop(value: &str) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(value),
        value,
        GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    )
}

fn pattern(value: &str) -> TransitPattern {
    TransitPattern::fixture(
        TransitPatternId::new(value),
        TransitMode::Bus,
        value,
        "destination",
        vec![
            BoardingPointId::new("origin"),
            BoardingPointId::new("destination"),
        ],
    )
}

fn scheduled_trip(value: &str, pattern_id: &str, duration: u32) -> TransitTrip {
    TransitTrip::fixture_weekdays(
        TransitTripId::new(value),
        TransitPatternId::new(pattern_id),
        vec![Weekday::Monday],
        vec![
            ScheduledStopTime::new(0, 0),
            ScheduledStopTime::new(duration, duration),
        ],
    )
}

fn effective_trip(value: &str, pattern_id: &str, arrival_minute: i64) -> EffectiveTrip {
    EffectiveTrip {
        id: EffectiveTripId::new(value),
        pattern_id: TransitPatternId::new(pattern_id),
        scheduled_trip_id: None,
        stop_times: vec![
            EffectiveStopTime {
                stop_id: BoardingPointId::new("origin"),
                arrival_at: datetime!(2026-09-21 10:01 UTC),
                departure_at: datetime!(2026-09-21 10:01 UTC),
            },
            EffectiveStopTime {
                stop_id: BoardingPointId::new("destination"),
                arrival_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(arrival_minute),
                departure_at: datetime!(2026-09-21 10:00 UTC) + Duration::minutes(arrival_minute),
            },
        ],
        timing_basis: TimingBasis::RealtimePrediction,
        timing_reliability: TimingReliability::Reliable,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    }
}

fn network() -> Arc<TransitRuntime> {
    let network = TransitNetwork::fixture(
        Time::from_hms(4, 0, 0).unwrap(),
        vec![stop("origin"), stop("destination")],
        vec![pattern("static-fast"), pattern("realtime-fast")],
        vec![
            scheduled_trip("static-fast-trip", "static-fast", 100),
            scheduled_trip("realtime-fast-trip", "realtime-fast", 200),
        ],
    );
    Arc::new(TransitRuntime::new(Arc::new(network)))
}

fn multiple_mode_corridors_network() -> Arc<TransitRuntime> {
    let pattern = |id: &str, mode, from: &str, to: &str| {
        TransitPattern::fixture(
            TransitPatternId::new(id),
            mode,
            id,
            to,
            vec![BoardingPointId::new(from), BoardingPointId::new(to)],
        )
    };
    let network = TransitNetwork::fixture(
        Time::from_hms(4, 0, 0).unwrap(),
        [
            "origin",
            "bus-end-a",
            "metro-start-a",
            "bus-end-b",
            "metro-start-b",
            "destination",
        ]
        .into_iter()
        .map(stop)
        .collect(),
        vec![
            pattern("bus-a", TransitMode::Bus, "origin", "bus-end-a"),
            pattern(
                "metro-a",
                TransitMode::Metro,
                "metro-start-a",
                "destination",
            ),
            pattern("bus-b", TransitMode::Bus, "origin", "bus-end-b"),
            pattern(
                "metro-b",
                TransitMode::Metro,
                "metro-start-b",
                "destination",
            ),
        ],
        Vec::new(),
    )
    .with_transfer_edges(vec![
        crate::domain::transit::TransferEdge::new(
            BoardingPointId::new("bus-end-a"),
            BoardingPointId::new("metro-start-a"),
            60,
            100,
        ),
        crate::domain::transit::TransferEdge::new(
            BoardingPointId::new("bus-end-b"),
            BoardingPointId::new("metro-start-b"),
            60,
            100,
        ),
    ]);
    Arc::new(TransitRuntime::new(Arc::new(network)))
}

fn test_planner(
    network: Arc<TransitRuntime>,
    timing: Arc<dyn TimingSnapshotLoader>,
) -> MultimodalPlanner {
    MultimodalPlanner::new(
        network,
        timing,
        Arc::new(FakeStreetRouter::builder().build()),
        Arc::new(BikeShareNetwork::new(Vec::new())),
        Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
        crate::config::ViaConfig::default().bike_policy(),
    )
}

fn query(k: usize) -> PlanningQuery {
    PlanningQuery::new(
        Coordinate::try_new(25.0, 121.5).unwrap(),
        Coordinate::try_new(25.0, 121.5).unwrap(),
        super::super::model::PlanningDeadline::after(std::time::Duration::from_secs(60)),
        vec![AccessPoint::new(
            BoardingPointId::new("origin"),
            datetime!(2026-09-21 10:00 UTC),
            datetime!(2026-09-21 10:00 UTC),
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("destination"), 0, 0)],
        datetime!(2026-09-21 10:00 UTC),
        crate::config::ViaConfig::default().planner_limits(),
    )
    .with_allowed_transit_modes([TransitMode::Bus])
    .with_limits(crate::routing::PlannerLimits {
        max_transfers: 0,
        max_candidates: 16,
        max_departures_per_query: 3,
        max_journey_duration_seconds: 4 * 60 * 60,
        ..crate::routing::PlannerLimits::default()
    })
    .with_max_static_corridors(k)
}

#[tokio::test]
async fn conflicting_duplicates_and_inconsistent_access_times_are_rejected() {
    let loader = Arc::new(RecordingTimingLoader::default());
    let planner = test_planner(network(), loader.clone());
    for modes in [vec![TransitMode::Bus], Vec::new()] {
        for case in 0..4 {
            let mut invalid = query(1).with_allowed_transit_modes(modes.iter().copied());
            match case {
                0 => {
                    let mut duplicate = invalid.origin_access[0].clone();
                    duplicate.distance_metres += 1;
                    invalid.origin_access.push(duplicate);
                }
                1 => {
                    let mut duplicate = invalid.destination_egress[0].clone();
                    duplicate.walking_seconds += 1;
                    invalid.destination_egress.push(duplicate);
                }
                2 => invalid.origin_access[0].available_at += Duration::seconds(1),
                _ => invalid.origin_access[0].departure_at -= Duration::seconds(1),
            }
            assert!(matches!(
                planner.plan_with_stats(invalid).await,
                Err(RoutingError::InvalidQuery)
            ));
        }
    }
    assert!(loader.queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn timing_queries_include_all_patterns_in_a_retained_pattern_family() {
    let loader = Arc::new(RecordingTimingLoader {
        trips: HashMap::from([
            (
                TransitPatternId::new("static-fast"),
                vec![effective_trip("static-fast-live", "static-fast", 20)],
            ),
            (
                TransitPatternId::new("realtime-fast"),
                vec![effective_trip("realtime-fast-live", "realtime-fast", 10)],
            ),
        ]),
        ..RecordingTimingLoader::default()
    });
    let planner = test_planner(network(), loader.clone());

    let (_, stats) = planner.plan_with_stats(query(1)).await.unwrap();

    let queries = loader.queries.lock().unwrap();
    assert_eq!(queries.len(), 2);
    assert!(
        queries
            .iter()
            .any(|query| query.pattern_id == TransitPatternId::new("static-fast"))
    );
    assert!(
        queries
            .iter()
            .any(|query| query.pattern_id == TransitPatternId::new("realtime-fast"))
    );
    assert_eq!(stats.phase1.corridor_positions, 2);
    assert_eq!(stats.phase1.corridors_retained, 1);
}

struct SlowTimingLoader;

#[async_trait::async_trait]
impl TimingSnapshotLoader for SlowTimingLoader {
    async fn load_snapshot(&self, _queries: Vec<DepartureQuery>) -> TimingSnapshot {
        tokio::time::sleep(std::time::Duration::from_secs(11)).await;
        TimingSnapshot::default()
    }
}

#[tokio::test(start_paused = true)]
async fn disabled_deadline_awaits_slow_snapshot_loading() {
    let planner = test_planner(network(), Arc::new(SlowTimingLoader));
    let mut query = query(1);
    query.deadline = crate::routing::PlanningDeadline::disabled();
    let started = tokio::time::Instant::now();
    assert!(planner.prepare(query).await.is_ok());
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(11));
}

#[tokio::test(start_paused = true)]
async fn enabled_deadline_cancels_slow_snapshot_loading() {
    let planner = test_planner(network(), Arc::new(SlowTimingLoader));
    let mut query = query(1);
    query.deadline = crate::routing::PlanningDeadline::after(std::time::Duration::from_secs(5));
    let started = tokio::time::Instant::now();
    assert!(matches!(
        planner.prepare(query).await,
        Err(RoutingError::DeadlineExceeded)
    ));
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn expired_deadline_stops_before_loading_timing_snapshot() {
    let loader = Arc::new(RecordingTimingLoader::default());
    let planner = test_planner(network(), loader.clone());
    let mut query = query(1);
    query.deadline = super::super::model::PlanningDeadline::elapsed_for_test();

    let result = planner.plan_with_stats(query).await;

    assert_eq!(result.unwrap_err(), RoutingError::DeadlineExceeded);
    assert_eq!(loader.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn phase2_chooses_actual_best_journey_within_retained_corridors() {
    let loader = Arc::new(RecordingTimingLoader {
        trips: HashMap::from([
            (
                TransitPatternId::new("static-fast"),
                vec![effective_trip("static-fast-live", "static-fast", 20)],
            ),
            (
                TransitPatternId::new("realtime-fast"),
                vec![effective_trip("realtime-fast-live", "realtime-fast", 10)],
            ),
        ]),
        ..RecordingTimingLoader::default()
    });
    let planner = test_planner(network(), loader);

    let (candidates, stats) = planner.plan_with_stats(query(2)).await.unwrap();

    assert_eq!(stats.phase1.corridors_retained, 1);
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[1].transit_pattern_sequence(),
        vec![TransitPatternId::new("static-fast")]
    );
    assert_eq!(
        candidates[0].transit_pattern_sequence(),
        vec![TransitPatternId::new("realtime-fast")]
    );
}

#[tokio::test]
async fn operational_metrics_count_completed_stages_once() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _guard = ::metrics::set_default_local_recorder(&recorder);
    let loader = Arc::new(RecordingTimingLoader {
        trips: HashMap::from([(
            TransitPatternId::new("static-fast"),
            vec![effective_trip("static-fast-live", "static-fast", 20)],
        )]),
        ..RecordingTimingLoader::default()
    });
    let origin = Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = Coordinate::try_new(25.001, 121.501).unwrap();
    let planner = test_planner(network(), loader);
    let mut planning_query = query(1);
    planning_query.origin = origin;
    planning_query.destination = destination;
    let session = planner.prepare(planning_query).await.unwrap();
    let report = session.evaluate().await;
    assert!(!report.outcome.unwrap().is_empty());
    let lookups = report.stats.phase2_evaluation.transit_departure_lookups;
    assert!(lookups > 0);
    let rendered = handle.render();
    assert!(rendered.contains(&format!(
        "via_planner_transit_departure_lookups_total {lookups}\n"
    )));
    assert!(rendered.contains("via_planner_transit_reachable_boardings_total 2\n"));
    assert!(rendered.contains("via_planner_transit_reachable_patterns_total 2\n"));
    assert!(rendered.contains("via_planner_transit_realtime_stops_requested_total 1\n"));
}

#[tokio::test]
async fn bike_availability_snapshot_is_loaded_once_per_planning_request() {
    let timing = Arc::new(RecordingTimingLoader::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let requested = Arc::new(Mutex::new(Vec::new()));
    let stop_names = [
        "origin",
        "bus-end-a",
        "metro-start-a",
        "bus-end-b",
        "metro-start-b",
        "destination",
    ];
    let mut stations = Vec::new();
    let mut links = Vec::new();
    for name in stop_names {
        let station_id = BikeStationId::new(format!("linked-{name}"));
        stations.push(crate::domain::bikeshare::BikeStation::new(
            station_id.clone(),
            name,
            GeoCoordinate {
                latitude: 24.0,
                longitude: 121.0,
            },
        ));
        links.push(crate::domain::bikeshare::BikeStationAccess::new(
            BoardingPointId::new(name),
            station_id,
            Some(crate::domain::bikeshare::BikeWalkingAccess::new(10, 10)),
            None,
        ));
    }
    stations.push(crate::domain::bikeshare::BikeStation::new(
        BikeStationId::new("origin-nearby"),
        "Origin nearby",
        GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    ));
    stations.push(crate::domain::bikeshare::BikeStation::new(
        BikeStationId::new("destination-nearby"),
        "Destination nearby",
        GeoCoordinate {
            latitude: 25.1,
            longitude: 121.5,
        },
    ));
    let planner = MultimodalPlanner::new(
        multiple_mode_corridors_network(),
        timing.clone(),
        Arc::new(FakeStreetRouter::builder().build()),
        Arc::new(
            BikeShareNetwork::new(stations)
                .with_access_links(links)
                .with_revision("fixture"),
        ),
        Arc::new(CountingBikeAvailabilitySource {
            calls: calls.clone(),
            requested: requested.clone(),
        }),
        crate::config::ViaConfig::default().bike_policy(),
    );
    let mut bike_query = query(2)
        .with_allowed_transit_modes([TransitMode::Bus, TransitMode::Metro])
        .with_limits(crate::routing::PlannerLimits {
            max_transfers: 1,
            max_candidates: 16,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
            ..crate::routing::PlannerLimits::default()
        })
        .with_bike_share(true);

    bike_query.destination = Coordinate::try_new(25.1, 121.5).unwrap();
    let (_, stats) = planner.plan_with_stats(bike_query).await.unwrap();

    assert_eq!(stats.phase1.corridors_retained, 2);
    let loaded = requested.lock().unwrap().clone();
    assert_eq!(loaded.len(), 8);
    for name in stop_names {
        assert!(loaded.contains(&BikeStationId::new(format!("linked-{name}"))));
    }
    assert!(loaded.contains(&BikeStationId::new("origin-nearby")));
    assert!(loaded.contains(&BikeStationId::new("destination-nearby")));
    assert_eq!(timing.calls.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    planner.plan_with_stats(query(1)).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn walking_is_configured_independently_of_bike_share() {
    let timing = Arc::new(RecordingTimingLoader::default());
    let origin = Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = Coordinate::try_new(25.001, 121.501).unwrap();
    let street = Arc::new(
        FakeStreetRouter::builder()
            .path(
                crate::street::StreetTravelMode::Walking,
                origin,
                destination,
                crate::street::StreetPath {
                    duration: crate::street::Seconds(60),
                    distance: crate::street::Metres(100),
                    geometry: vec![origin, destination],
                    maneuvers: Vec::new(),
                    attribution: Vec::new(),
                },
            )
            .build(),
    );
    let planner = MultimodalPlanner::new(
        network(),
        timing.clone(),
        street,
        Arc::new(BikeShareNetwork::new(Vec::new())),
        Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
        crate::config::ViaConfig::default().bike_policy(),
    );
    let query = PlanningQuery::new(
        origin,
        destination,
        super::super::model::PlanningDeadline::after(std::time::Duration::from_secs(60)),
        Vec::new(),
        Vec::new(),
        datetime!(2026-09-21 10:00 UTC),
        crate::config::ViaConfig::default().planner_limits(),
    )
    .with_allowed_transit_modes([]);
    let (journeys, stats) = planner.plan_with_stats(query).await.unwrap();
    assert_eq!(journeys.len(), 1);
    assert_eq!(stats.phase1.states_expanded, 0);
    assert_eq!(timing.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn direct_walking_is_evaluated_once_without_transit() {
    let timing = Arc::new(RecordingTimingLoader::default());
    let origin = Coordinate::try_new(25.0, 121.5).unwrap();
    let destination = Coordinate::try_new(25.001, 121.501).unwrap();
    let street = Arc::new(
        FakeStreetRouter::builder()
            .path(
                crate::street::StreetTravelMode::Walking,
                origin,
                destination,
                crate::street::StreetPath {
                    duration: crate::street::Seconds(60),
                    distance: crate::street::Metres(100),
                    geometry: vec![origin, destination],
                    maneuvers: Vec::new(),
                    attribution: Vec::new(),
                },
            )
            .build(),
    );
    let planner = MultimodalPlanner::new(
        network(),
        timing,
        street,
        Arc::new(BikeShareNetwork::new(Vec::new())),
        Arc::new(crate::providers::UnavailableBikeAvailabilitySource),
        crate::config::ViaConfig::default().bike_policy(),
    );
    let query = PlanningQuery::new(
        origin,
        destination,
        super::super::model::PlanningDeadline::after(std::time::Duration::from_secs(60)),
        Vec::new(),
        Vec::new(),
        datetime!(2026-09-21 10:00 UTC),
        crate::config::ViaConfig::default().planner_limits(),
    )
    .with_allowed_transit_modes([]);
    let session = planner.prepare(query).await.unwrap();

    let report = session.evaluate().await;
    let primary = report.outcome.unwrap();

    assert_eq!(primary.len(), 1);
    assert_eq!(report.stats.phase2_evaluation.states_expanded, 1);
}
