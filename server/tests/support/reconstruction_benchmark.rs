use super::*;
use crate::{
    domain::{
        bikeshare::{BikeShareNetwork, BikeStation, BikeWalkingAccess},
        ids::BikeStationId,
        location::GeoCoordinate,
        transit::TransitNetwork,
    },
    routing::{
        BikeRide, BikeShareMovement, JourneyAnchor, JourneyAnchorKind, JourneyMovement,
        JourneyMovementKind, WalkingMovementKind,
    },
    street::{MatrixQuery, MatrixResult, Metres, RouteQuery, Seconds, StreetPath},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use time::{Duration, macros::datetime};
use tokio::sync::Semaphore;

struct DelayedRouter {
    delay: u64,
    semaphore: Semaphore,
    active: AtomicUsize,
    maximum: AtomicUsize,
    attempts: AtomicUsize,
    wait_ns: AtomicUsize,
}
#[async_trait::async_trait]
impl StreetRouter for DelayedRouter {
    async fn matrix(&self, _: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
        unreachable!()
    }
    async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
        let start = Instant::now();
        let _permit = self.semaphore.acquire().await.unwrap();
        self.wait_ns
            .fetch_add(start.elapsed().as_nanos() as usize, Ordering::Relaxed);
        self.attempts.fetch_add(1, Ordering::Relaxed);
        let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
        self.maximum.fetch_max(active, Ordering::Relaxed);
        if self.delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay)).await;
        }
        self.active.fetch_sub(1, Ordering::Relaxed);
        Ok(StreetPath {
            duration: Seconds(60),
            distance: Metres(100),
            geometry: vec![query.origin, query.destination],
            maneuvers: vec![],
            attribution: vec![],
        })
    }
}

#[tokio::test]
#[ignore]
async fn reconstruction_concurrency_measurement() {
    let departure = datetime!(2026-08-31 10:00 +08:00);
    let geo = |longitude| GeoCoordinate {
        latitude: 25.0,
        longitude,
    };
    let request: PlanningRequest = serde_json::from_value(serde_json::json!({"origin":{"coordinate":geo(121.5),"display_text":"origin"},"destination":{"coordinate":geo(121.51),"display_text":"destination"},"selected_modes":["bus","metro","bike_share"],"departure_setting":{"type":"depart_now"}})).unwrap();
    let context =
        JourneyPlanningContext::new(&request, departure, 14400, PlanningDeadline::disabled())
            .unwrap();
    let stations = vec![
        BikeStation::new(BikeStationId::new("pickup"), "pickup", geo(121.502)),
        BikeStation::new(BikeStationId::new("dropoff"), "dropoff", geo(121.508)),
    ];
    let runtime = Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::default())));
    let network = Arc::new(BikeShareNetwork::new(stations));
    println!(
        "BENCH,workload,candidate_count,delay_ms,concurrency,repetition,wall_ms,successful,rejected,errors,route_calls,route_timeouts,semaphore_wait_ms,max_active_provider,output_digest"
    );
    for bike in [false, true] {
        let kind = if bike {
            JourneyMovementKind::BikeShare {
                movement: BikeShareMovement::new(
                    0,
                    1,
                    BikeStationId::new("pickup"),
                    BikeStationId::new("dropoff"),
                    BikeWalkingAccess::new(60, 100),
                    BikeRide {
                        duration_seconds: 60,
                        distance_metres: 100,
                    },
                    BikeWalkingAccess::new(60, 100),
                )
                .unwrap(),
            }
        } else {
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Direct,
                duration_seconds: 60,
                distance_metres: 100,
            }
        };
        let duration = if bike { 180 } else { 60 };
        let journey = RealizedJourney::from_movements(
            vec![
                JourneyAnchor {
                    kind: JourneyAnchorKind::Origin,
                    reached_at: departure,
                },
                JourneyAnchor {
                    kind: JourneyAnchorKind::Destination,
                    reached_at: departure + Duration::seconds(duration),
                },
            ],
            vec![JourneyMovement {
                from_anchor: 0,
                to_anchor: 1,
                departure_at: departure,
                arrival_at: departure + Duration::seconds(duration),
                kind,
            }],
            departure,
        )
        .unwrap();
        let journeys = vec![journey; 16];
        for delay in [0, 10, 50, 100] {
            let mut reference = None;
            for concurrency in [1, 2, 4, 8, 16] {
                for repetition in 0..23 {
                    let router = Arc::new(DelayedRouter {
                        delay,
                        semaphore: Semaphore::new(8),
                        active: AtomicUsize::new(0),
                        maximum: AtomicUsize::new(0),
                        attempts: AtomicUsize::new(0),
                        wait_ns: AtomicUsize::new(0),
                    });
                    let reconstructor =
                        JourneyReconstructor::new(router.clone(), runtime.clone(), network.clone());
                    let start = Instant::now();
                    let results = ordered_reconstruction(
                        journeys
                            .iter()
                            .map(|j| reconstructor.reconstruct(j, &context)),
                        concurrency,
                    )
                    .collect::<Vec<_>>()
                    .await;
                    let wall = start.elapsed().as_secs_f64() * 1000.0;
                    let options = results
                        .into_iter()
                        .map(|r| r.unwrap().unwrap().option)
                        .collect::<Vec<_>>();
                    let bytes = serde_json::to_vec(&options).unwrap();
                    use sha2::{Digest, Sha256};
                    let digest = format!("{:x}", Sha256::digest(&bytes));
                    if let Some(expected) = &reference {
                        assert_eq!(&digest, expected);
                    } else {
                        reference = Some(digest.clone());
                    }
                    if repetition >= 3 {
                        println!(
                            "BENCH,{},{},{},{},{},{:.6},{},0,0,{},0,{:.6},{},{}",
                            if bike {
                                "bikeshare_surrogate"
                            } else {
                                "walking_surrogate"
                            },
                            journeys.len(),
                            delay,
                            concurrency,
                            repetition - 3,
                            wall,
                            options.len(),
                            router.attempts.load(Ordering::Relaxed),
                            router.wait_ns.load(Ordering::Relaxed) as f64 / 1e6,
                            router.maximum.load(Ordering::Relaxed),
                            digest
                        );
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct LiveObserver(std::sync::Mutex<Vec<crate::providers::valhalla::OperationalEvent>>);

impl crate::providers::valhalla::OperationalEventObserver for LiveObserver {
    fn observe(&self, event: crate::providers::valhalla::OperationalEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[tokio::test]
#[ignore]
async fn reconstruction_valhalla_pilot() {
    use crate::providers::valhalla::{OperationOutcome, ValhallaConfig, ValhallaProvider};
    use sha2::{Digest, Sha256};
    let observer = Arc::new(LiveObserver::default());
    let provider = Arc::new(
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
        .unwrap(),
    );
    let departure = datetime!(2026-08-31 10:00 +08:00);
    let geo = |longitude| GeoCoordinate {
        latitude: 25.0,
        longitude,
    };
    let request: PlanningRequest = serde_json::from_value(serde_json::json!({"origin":{"coordinate":geo(121.5),"display_text":"origin"},"destination":{"coordinate":geo(121.51),"display_text":"destination"},"selected_modes":["bus","metro","bike_share"],"departure_setting":{"type":"depart_now"}})).unwrap();
    let context =
        JourneyPlanningContext::new(&request, departure, 14400, PlanningDeadline::disabled())
            .unwrap();
    let stations = vec![
        BikeStation::new(BikeStationId::new("pickup"), "pickup", geo(121.502)),
        BikeStation::new(BikeStationId::new("dropoff"), "dropoff", geo(121.508)),
    ];
    let reconstructor = JourneyReconstructor::new(
        provider,
        Arc::new(TransitRuntime::new(Arc::new(TransitNetwork::default()))),
        Arc::new(BikeShareNetwork::new(stations)),
    );
    println!(
        "LIVE,workload,candidate_count,concurrency,repetition,wall_ms,successful,rejected,errors,route_operations,route_successes,route_timeouts,route_operation_total_ms,output_digest"
    );
    for bike in [false, true] {
        let kind = if bike {
            JourneyMovementKind::BikeShare {
                movement: BikeShareMovement::new(
                    0,
                    1,
                    BikeStationId::new("pickup"),
                    BikeStationId::new("dropoff"),
                    BikeWalkingAccess::new(60, 100),
                    BikeRide {
                        duration_seconds: 60,
                        distance_metres: 100,
                    },
                    BikeWalkingAccess::new(60, 100),
                )
                .unwrap(),
            }
        } else {
            JourneyMovementKind::Walking {
                kind: WalkingMovementKind::Direct,
                duration_seconds: 60,
                distance_metres: 100,
            }
        };
        let duration = if bike { 180 } else { 60 };
        let journey = RealizedJourney::from_movements(
            vec![
                JourneyAnchor {
                    kind: JourneyAnchorKind::Origin,
                    reached_at: departure,
                },
                JourneyAnchor {
                    kind: JourneyAnchorKind::Destination,
                    reached_at: departure + Duration::seconds(duration),
                },
            ],
            vec![JourneyMovement {
                from_anchor: 0,
                to_anchor: 1,
                departure_at: departure,
                arrival_at: departure + Duration::seconds(duration),
                kind,
            }],
            departure,
        )
        .unwrap();
        let journeys = vec![journey; 16];
        for concurrency in [1, 2, 4, 8, 16] {
            for repetition in 0..4 {
                observer.0.lock().unwrap().clear();
                let started = Instant::now();
                let results = ordered_reconstruction(
                    journeys
                        .iter()
                        .map(|journey| reconstructor.reconstruct(journey, &context)),
                    concurrency,
                )
                .collect::<Vec<_>>()
                .await;
                let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
                let mut options = Vec::new();
                let mut rejected = 0;
                let mut errors = 0;
                for result in results {
                    match result {
                        Ok(Some(candidate)) => options.push(candidate.option),
                        Ok(None) => rejected += 1,
                        Err(_) => errors += 1,
                    }
                }
                let events = observer.0.lock().unwrap();
                let successes = events
                    .iter()
                    .filter(|e| e.outcome == OperationOutcome::Success)
                    .count();
                let timeouts = events
                    .iter()
                    .filter(|e| e.outcome == OperationOutcome::Timeout)
                    .count();
                let duration: u64 = events.iter().map(|e| e.latency_milliseconds).sum();
                let digest = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&options).unwrap())
                );
                if repetition > 0 {
                    println!(
                        "LIVE,{},{},{},{},{:.3},{},{},{},{},{},{},{},{}",
                        if bike {
                            "bikeshare_surrogate"
                        } else {
                            "walking_surrogate"
                        },
                        journeys.len(),
                        concurrency,
                        repetition - 1,
                        wall_ms,
                        options.len(),
                        rejected,
                        errors,
                        events.len(),
                        successes,
                        timeouts,
                        duration,
                        digest
                    );
                }
            }
        }
    }
}
