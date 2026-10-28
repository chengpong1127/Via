use super::*;
use crate::{
    domain::{
        bikeshare::{BikeAvailabilitySnapshot, BikeStationAvailability},
        ids::BikeStationId,
        transit_timing::{
            DepartureQuery, ScheduleTiming, SnapshotDepartures, TimingReliability, TimingSnapshot,
            TimingSnapshotLoader,
        },
    },
    providers::{
        BikeAvailabilityError, BikeAvailabilitySource,
        valhalla::{
            OperationOutcome, OperationalEvent, OperationalEventObserver, ValhallaConfig,
            ValhallaProvider, measurement::RouteMeasurement,
        },
    },
};

struct StaticTiming(ScheduleTiming);
#[async_trait::async_trait]
impl TimingSnapshotLoader for StaticTiming {
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
            .map(|id| {
                (
                    id.clone(),
                    BikeStationAvailability::new(id.clone(), 5, 5, true, now),
                )
            })
            .collect();
        Ok(Arc::new(BikeAvailabilitySnapshot::new(now, stations)))
    }
}

#[tokio::test]
#[ignore]
async fn capture_representative_pools() {
    let path = std::path::Path::new("data/build/20261001T173300Z/.via-static.json");
    let artifact = crate::data::load_static_artifact(path).unwrap();
    let runtime = Arc::new(TransitRuntime::new(Arc::new(artifact.transit)));
    let network = Arc::new(artifact.bike_share);
    let settings =
        crate::config::ViaConfig::load(Some(std::path::Path::new("config/via.toml"))).unwrap();
    let limits = settings.journey_limits();
    let metrics = Arc::new(RouteMeasurement::default());
    let observer = Arc::new(RouteObserver::default());
    let street = Arc::new(
        ValhallaProvider::with_observer(
            ValhallaConfig {
                base_url: "http://valhalla:8002/".parse().unwrap(),
                request_timeout: std::time::Duration::from_millis(1200),
                max_concurrency: 8,
                max_sources: 32,
                max_targets: 32,
                max_response_bytes: 2_097_152,
            },
            observer.clone(),
        )
        .unwrap()
        .with_measurement(metrics.clone()),
    );
    let planner = MultimodalPlanner::new(
        runtime.clone(),
        Arc::new(StaticTiming(ScheduleTiming::new(runtime.clone()))),
        street.clone(),
        network.clone(),
        Arc::new(AvailableBikes),
        limits.bike.clone(),
    );
    let departure = time::macros::datetime!(2026-10-01 10:00 +08:00);
    for bike in [false, true] {
        if bike && std::env::var_os("VIA_REP_TRANSIT_FULL_ONLY").is_some() {
            continue;
        }
        if !bike && std::env::var_os("VIA_REP_MIXED_ONLY").is_some() {
            continue;
        }
        let modes = if bike {
            serde_json::json!(["bus", "metro", "bike_share"])
        } else {
            serde_json::json!(["bus", "metro"])
        };
        let request: PlanningRequest = serde_json::from_value(serde_json::json!({"origin":{"coordinate":{"latitude":25.0478,"longitude":121.5170},"display_text":"Taipei Main Station"},"destination":{"coordinate":{"latitude":25.0338,"longitude":121.5645},"display_text":"Xinyi"},"selected_modes":modes,"departure_setting":{"type":"depart_now"}})).unwrap();
        let context = JourneyPlanningContext::new(
            &request,
            departure,
            limits.planner.max_journey_duration_seconds,
            PlanningDeadline::disabled(),
        )
        .unwrap();
        let allowed = request.allowed_transit_modes();
        let access = planner
            .discover_access_points(
                street.as_ref(),
                request.origin.coordinate,
                departure,
                context.access_constraints(),
                &allowed,
                &limits,
            )
            .await
            .unwrap();
        let egress = planner
            .discover_egress_points(
                street.as_ref(),
                request.destination.coordinate,
                context.direct_constraints(),
                &allowed,
                &limits,
            )
            .await
            .unwrap();
        println!(
            "CAPTURE access={} egress={} bike={bike}",
            access.len(),
            egress.len()
        );
        let query = PlanningQuery::new(
            to_street_coordinate(request.origin.coordinate).unwrap(),
            to_street_coordinate(request.destination.coordinate).unwrap(),
            PlanningDeadline::disabled(),
            access,
            egress,
            departure,
            limits.planner,
        )
        .with_allowed_transit_modes(allowed)
        .with_bike_share(bike)
        .with_limits(limits.planner);
        let mut session = planner.prepare(query).await.unwrap();
        let journeys = session.evaluate().await.unwrap();
        let transit = journeys
            .iter()
            .filter(|j| !j.transit_pattern_sequence().is_empty())
            .count();
        let bikes = journeys.iter().filter(|j| j.has_bike_share()).count();
        println!(
            "CAPTURE bike={bike} total={} transit={transit} bike_candidates={bikes} stats={:?}",
            journeys.len(),
            session.stats()
        );
        drop(session);
        let reconstructor =
            JourneyReconstructor::new(street.clone(), runtime.clone(), network.clone());
        let service = RoutingService {
            street_routing: street.clone(),
            planner: planner.clone(),
            reconstructor: reconstructor.clone(),
            ranking: limits.ranking.clone(),
            limits: limits.clone(),
            request_timeout: None,
            request_slots: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_requests)),
        };
        let mut sizes = [16, 64, 256]
            .into_iter()
            .filter(|size| *size < journeys.len())
            .collect::<Vec<_>>();
        if std::env::var_os("VIA_REP_TRANSIT_FULL_ONLY").is_some() {
            sizes.clear();
        }
        sizes.push(journeys.len());
        for size in sizes {
            let indices = (0..size)
                .map(|i| {
                    if size == 1 {
                        0
                    } else {
                        i * (journeys.len() - 1) / (size - 1)
                    }
                })
                .collect::<Vec<_>>();
            let pool = indices.iter().map(|i| &journeys[*i]).collect::<Vec<_>>();
            replay(
                &pool,
                &context,
                &service,
                &reconstructor,
                &metrics,
                &observer,
                (bike, size == journeys.len()),
            )
            .await;
        }
    }
}

#[derive(Default)]
struct RouteObserver(std::sync::Mutex<Vec<OperationalEvent>>);
impl OperationalEventObserver for RouteObserver {
    fn observe(&self, event: OperationalEvent) {
        self.0.lock().unwrap().push(event);
    }
}

struct ActiveGuard<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn replay(
    pool: &[&RealizedJourney],
    context: &JourneyPlanningContext<'_>,
    service: &RoutingService,
    reconstructor: &JourneyReconstructor,
    metrics: &Arc<RouteMeasurement>,
    observer: &Arc<RouteObserver>,
    workload: (bool, bool),
) {
    let (bike, full) = workload;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let identity = format!(
        "{:x}",
        Sha256::digest(
            pool.iter()
                .map(|journey| journey.identity().canonical_signature())
                .collect::<Vec<_>>()
                .join("\n")
                .as_bytes()
        )
    );
    let (warmups, measured) = if std::env::var_os("VIA_REP_TRANSIT_FULL_ONLY").is_some() {
        (1, 1)
    } else if pool.len() == 16 {
        (3, 20)
    } else if pool.len() == 64 {
        (3, if bike { 3 } else { 5 })
    } else {
        (1, if bike { 1 } else { 2 })
    };
    for round in 0..warmups + measured {
        for index in 0..5 {
            let concurrency = [1, 2, 4, 8, 16][(index + round) % 5];
            metrics.reset();
            observer.0.lock().unwrap().clear();
            let active = AtomicUsize::new(0);
            let maximum = AtomicUsize::new(0);
            let started = Instant::now();
            let results = ordered_reconstruction(
                pool.iter().map(|journey| {
                    let active = &active;
                    let maximum = &maximum;
                    async move {
                        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(count, Ordering::SeqCst);
                        let _guard = ActiveGuard(active);
                        reconstructor.reconstruct(journey, context).await
                    }
                }),
                concurrency,
            )
            .collect::<Vec<_>>()
            .await;
            let wall = started.elapsed().as_secs_f64() * 1000.0;
            let mut fingerprint = Sha256::new();
            let mut candidates = Vec::new();
            let mut rejected = 0;
            let mut failed = 0;
            for result in results {
                match result {
                    Ok(Some(candidate)) => {
                        fingerprint.update(format!("{:?}", candidate.ranking).as_bytes());
                        fingerprint.update(serde_json::to_vec(&candidate.option).unwrap());
                        candidates.push(candidate);
                    }
                    Ok(None) => {
                        fingerprint.update(b"rejected");
                        rejected += 1;
                    }
                    Err(error) => {
                        fingerprint.update(format!("error:{error:?}").as_bytes());
                        failed += 1;
                    }
                }
            }
            let successful = candidates.len();
            let candidate_digest = format!("{:x}", fingerprint.finalize());
            let outcome = service
                .rank_candidates(context.search_start, candidates)
                .unwrap();
            let top3_keys = match &outcome {
                PlanningOutcome::Options(options) => options
                    .journey_options
                    .iter()
                    .map(|option| option.matching_key.clone())
                    .collect::<Vec<_>>(),
                PlanningOutcome::Empty(_) => Vec::new(),
            };
            let top3_digest = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&outcome).unwrap())
            );
            let events = observer.0.lock().unwrap();
            let logical = events.len();
            let timeouts = events
                .iter()
                .filter(|event| event.outcome == OperationOutcome::Timeout)
                .count();
            let operation_ms: u64 = events.iter().map(|event| event.latency_milliseconds).sum();
            let waits = metrics.waits_us.lock().unwrap();
            let mut sorted = waits.clone();
            sorted.sort_unstable();
            let quantile = |percent: usize| -> f64 {
                if sorted.is_empty() {
                    0.0
                } else {
                    sorted[(sorted.len() - 1) * percent / 100] as f64 / 1000.0
                }
            };
            if round >= warmups {
                println!(
                    "REPRESENTATIVE {}",
                    serde_json::json!({"workload":if bike {"transit_bike"} else {"transit_only"},"pool_size":pool.len(),"full":full,"identity_digest":identity,"concurrency":concurrency,"repetition":round-warmups,"wall_ms":wall,"successful":successful,"rejected":rejected,"failed":failed,"logical_operations":logical,"operation_timeouts":timeouts,"operation_total_ms":operation_ms,"physical_http_attempts":metrics.attempts.load(Ordering::SeqCst),"http_responses":metrics.responses.load(Ordering::SeqCst),"http_successes":metrics.success.load(Ordering::SeqCst),"http_incomplete":metrics.incomplete.load(Ordering::SeqCst),"http_total_ms":metrics.http_us.load(Ordering::SeqCst) as f64/1000.0,"wait_attempts":metrics.wait_attempts.load(Ordering::SeqCst),"wait_total_ms":waits.iter().sum::<u64>() as f64/1000.0,"wait_p50_ms":quantile(50),"wait_p95_ms":quantile(95),"wait_max_ms":sorted.last().copied().unwrap_or(0) as f64/1000.0,"max_waiting":metrics.max_waiting.load(Ordering::SeqCst),"max_http_active":metrics.max_active.load(Ordering::SeqCst),"max_reconstruction_active":maximum.load(Ordering::SeqCst),"candidate_output_digest":candidate_digest,"top3_keys":top3_keys,"top3_output_digest":top3_digest})
                );
            }
        }
    }
}

#[tokio::test]
#[ignore]
async fn reconstruction_head_of_line_measurement() {
    use std::sync::Mutex;
    for slow_first in [true, false] {
        for repetition in 0..23 {
            let starts = Mutex::new(vec![0.0; 8]);
            let started = Instant::now();
            let results = ordered_reconstruction(
                (0..8).map(|index| {
                    let starts = &starts;
                    async move {
                        starts.lock().unwrap()[index] = started.elapsed().as_secs_f64() * 1000.0;
                        let slow = if slow_first { index == 0 } else { index == 7 };
                        tokio::time::sleep(std::time::Duration::from_millis(if slow {
                            80
                        } else {
                            5
                        }))
                        .await;
                        index
                    }
                }),
                4,
            )
            .collect::<Vec<_>>()
            .await;
            let wall = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(results, (0..8).collect::<Vec<_>>());
            if repetition >= 3 {
                println!(
                    "HEAD_OF_LINE {}",
                    serde_json::json!({"slow_first":slow_first,"repetition":repetition-3,"candidate_5_start_ms":starts.lock().unwrap()[4],"wall_ms":wall})
                );
            }
        }
    }
}
