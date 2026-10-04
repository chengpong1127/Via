use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::{Time, Weekday, macros::datetime};
use via_server::domain::{
    ids::{
        BoardingPointId, EffectiveTripId, ServiceCalendarId, TransitPatternId, TransitRouteId,
        TransitTripId,
    },
    transit::{
        ScheduledStopTime, SegmentProfile, SegmentTimeSource, SegmentTravelTime, ServiceCalendar,
        TransitMode, TransitNetwork, TransitPattern, TransitRoute, TransitTrip,
    },
    transit_runtime::TransitRuntime,
    transit_timing::{
        DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, SnapshotDepartures,
        TimingBasis, TimingReliability, TimingSnapshot, TimingSnapshotLoader,
    },
};
use via_server::providers::timing::{
    DepartureResolver, ThsrDepartureSource, TransitDepartureSource,
};

#[derive(Clone)]
struct RecordingSource {
    mode: TransitMode,
    calls: Arc<Mutex<Vec<TransitPatternId>>>,
}

#[async_trait]
impl TransitDepartureSource for RecordingSource {
    fn mode(&self) -> TransitMode {
        self.mode
    }

    fn static_timing_reliability(&self, _query: &DepartureQuery) -> TimingReliability {
        TimingReliability::Fallback
    }

    async fn static_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.calls.lock().unwrap().push(query.pattern_id.clone());
        Ok(vec![EffectiveTrip {
            id: EffectiveTripId::new(format!("fixture:{}", query.pattern_id.as_str())),
            pattern_id: query.pattern_id,
            scheduled_trip_id: None,
            stop_times: vec![
                EffectiveStopTime {
                    stop_id: BoardingPointId::new("board"),
                    arrival_at: query.after,
                    departure_at: query.after,
                },
                EffectiveStopTime {
                    stop_id: BoardingPointId::new("alight"),
                    arrival_at: query.after + time::Duration::minutes(5),
                    departure_at: query.after + time::Duration::minutes(5),
                },
            ],
            timing_basis: TimingBasis::Schedule,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            uncertainty_seconds: None,
            evidence: Vec::new(),
        }])
    }
}

struct DelayedSnapshotSource {
    mode: TransitMode,
    delay: std::time::Duration,
    entries: Vec<SnapshotDepartures>,
    batches: Arc<Mutex<Vec<Vec<DepartureQuery>>>>,
}

#[async_trait]
impl TransitDepartureSource for DelayedSnapshotSource {
    fn mode(&self) -> TransitMode {
        self.mode
    }

    fn static_timing_reliability(&self, _query: &DepartureQuery) -> TimingReliability {
        TimingReliability::Fallback
    }

    async fn static_departures(
        &self,
        _query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        Err(DepartureError::TemporarilyUnavailable)
    }

    async fn snapshot_batch(&self, queries: Vec<DepartureQuery>) -> Vec<SnapshotDepartures> {
        self.batches.lock().unwrap().push(queries);
        tokio::time::sleep(self.delay).await;
        self.entries.clone()
    }
}

#[tokio::test(start_paused = true)]
async fn snapshot_mode_sources_overlap_and_preserve_results() {
    let network = Arc::new(TransitRuntime::new(Arc::new(
        TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            vec![
                service("bus", TransitMode::Bus),
                service("metro", TransitMode::Metro),
            ],
            Vec::new(),
        )
        .with_routes(routes()),
    )));
    let after = datetime!(2026-08-31 10:00 UTC);
    let query = |id: &str, index| {
        DepartureQuery::at_position(
            TransitPatternId::new(id),
            BoardingPointId::new(if index == 0 { "board" } else { "alight" }),
            index,
            after,
            1,
        )
    };
    let bus_queries = vec![query("bus", 0), query("bus", 1)];
    let metro_queries = vec![query("metro", 0), query("metro", 1)];
    let mut sources = Vec::<Arc<dyn TransitDepartureSource>>::new();
    let mut batches = Vec::new();
    let mut expected = TimingSnapshot::default();
    for (mode, queries, seconds) in [
        (TransitMode::Bus, &bus_queries, 2),
        (TransitMode::Metro, &metro_queries, 1),
    ] {
        let recording = RecordingSource {
            mode,
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let mut entries = recording.snapshot_batch(queries.clone()).await;
        entries[0].preferred = recording
            .static_departures(queries[0].clone())
            .await
            .unwrap();
        entries[1] = SnapshotDepartures {
            failure: Some(if mode == TransitMode::Bus {
                DepartureError::TemporarilyUnavailable
            } else {
                DepartureError::InvalidQuery
            }),
            ..Default::default()
        };
        for (query, entry) in queries.iter().zip(&entries) {
            expected.insert_entry(
                query.pattern_id.clone(),
                query.board_index.unwrap(),
                entry.clone(),
            );
        }
        let source_batches = Arc::new(Mutex::new(Vec::new()));
        batches.push(source_batches.clone());
        sources.push(Arc::new(DelayedSnapshotSource {
            mode,
            delay: std::time::Duration::from_secs(seconds),
            entries,
            batches: source_batches,
        }));
    }
    let resolver = DepartureResolver::new(network, sources);
    let started = tokio::time::Instant::now();
    let snapshot = resolver
        .load_snapshot(vec![
            metro_queries[0].clone(),
            bus_queries[0].clone(),
            metro_queries[1].clone(),
            bus_queries[1].clone(),
        ])
        .await;

    assert_eq!(started.elapsed(), std::time::Duration::from_secs(2));
    assert_eq!(*batches[0].lock().unwrap(), vec![bus_queries.clone()]);
    assert_eq!(*batches[1].lock().unwrap(), vec![metro_queries.clone()]);
    for query in bus_queries.iter().chain(&metro_queries) {
        for at in [after, after + time::Duration::seconds(1)] {
            assert_eq!(
                snapshot.departures(&query.pattern_id, query.board_index.unwrap(), at, 10),
                expected.departures(&query.pattern_id, query.board_index.unwrap(), at, 10),
            );
        }
    }
    assert_eq!(snapshot.first_failure(), expected.first_failure());
    assert_eq!(
        snapshot.all_positions_failed(),
        expected.all_positions_failed()
    );
}

#[tokio::test(start_paused = true)]
async fn snapshot_preserves_invalid_queries_missing_sources_and_short_batches() {
    let network = Arc::new(TransitRuntime::new(Arc::new(
        TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            vec![
                service("bus", TransitMode::Bus),
                service("metro", TransitMode::Metro),
            ],
            Vec::new(),
        )
        .with_routes(routes()),
    )));
    let batches = Arc::new(Mutex::new(Vec::new()));
    let source = Arc::new(DelayedSnapshotSource {
        mode: TransitMode::Bus,
        delay: std::time::Duration::ZERO,
        entries: vec![SnapshotDepartures::default()],
        batches: batches.clone(),
    });
    let resolver = DepartureResolver::new(network, vec![source as Arc<dyn TransitDepartureSource>]);
    let query = |id: &str| {
        DepartureQuery::at_position(
            TransitPatternId::new(id),
            BoardingPointId::new("board"),
            0,
            datetime!(2026-08-31 10:00 UTC),
            1,
        )
    };
    let mut invalid_board = query("bus");
    invalid_board.board_index = None;
    let mut short_batch = query("bus");
    short_batch.board_index = Some(1);

    for (queries, failure) in [
        (vec![query("unknown")], DepartureError::InvalidQuery),
        (vec![query("metro")], DepartureError::InvalidConfiguration),
        (vec![invalid_board.clone()], DepartureError::InvalidQuery),
        (
            vec![query("bus"), short_batch.clone()],
            DepartureError::TemporarilyUnavailable,
        ),
    ] {
        let snapshot = resolver.load_snapshot(queries).await;
        assert_eq!(snapshot.first_failure(), Some(&failure));
        assert_eq!(
            snapshot.all_positions_failed(),
            failure != DepartureError::TemporarilyUnavailable
        );
    }
    assert_eq!(
        *batches.lock().unwrap(),
        vec![vec![invalid_board], vec![query("bus"), short_batch]]
    );
    let empty = resolver.load_snapshot(Vec::new()).await;
    assert_eq!(empty.first_failure(), None);
    assert!(!empty.all_positions_failed());
}

fn service(id: &str, _mode: TransitMode) -> TransitPattern {
    TransitPattern::new(
        TransitPatternId::new(id),
        TransitRouteId::new(id),
        None,
        "alight",
        vec![
            BoardingPointId::new("board"),
            BoardingPointId::new("alight"),
        ],
        SegmentProfile {
            segments: vec![SegmentTravelTime {
                seconds: 300,
                source: SegmentTimeSource::Schedule,
            }],
        },
    )
}

fn routes() -> Vec<TransitRoute> {
    [
        ("bus", TransitMode::Bus),
        ("metro", TransitMode::Metro),
        ("tra", TransitMode::Rail),
        ("thsr", TransitMode::HighSpeedRail),
    ]
    .into_iter()
    .map(|(id, mode)| TransitRoute {
        id: TransitRouteId::new(id),
        mode,
        label: id.into(),
    })
    .collect()
}

fn calendar() -> ServiceCalendar {
    ServiceCalendar {
        id: ServiceCalendarId::new("monday"),
        weekdays: vec![Weekday::Monday],
        start_date: None,
        end_date: None,
        added_dates: Vec::new(),
        removed_dates: Vec::new(),
    }
}

#[tokio::test]
async fn departure_resolver_routes_by_mode_and_restores_original_order() {
    let services = vec![
        service("bus", TransitMode::Bus),
        service("metro", TransitMode::Metro),
        service("tra", TransitMode::Rail),
        service("thsr", TransitMode::HighSpeedRail),
    ];
    let network = Arc::new(
        TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            services,
            Vec::new(),
        )
        .with_routes(routes()),
    );
    let network = Arc::new(TransitRuntime::new(network));
    let bus_calls = Arc::new(Mutex::new(Vec::new()));
    let metro_calls = Arc::new(Mutex::new(Vec::new()));
    let tra_calls = Arc::new(Mutex::new(Vec::new()));
    let thsr_calls = Arc::new(Mutex::new(Vec::new()));
    let resolver = DepartureResolver::new(
        network,
        vec![
            Arc::new(RecordingSource {
                mode: TransitMode::Bus,
                calls: bus_calls.clone(),
            }) as Arc<dyn TransitDepartureSource>,
            Arc::new(RecordingSource {
                mode: TransitMode::Metro,
                calls: metro_calls.clone(),
            }) as Arc<dyn TransitDepartureSource>,
            Arc::new(RecordingSource {
                mode: TransitMode::Rail,
                calls: tra_calls.clone(),
            }) as Arc<dyn TransitDepartureSource>,
            Arc::new(RecordingSource {
                mode: TransitMode::HighSpeedRail,
                calls: thsr_calls.clone(),
            }) as Arc<dyn TransitDepartureSource>,
        ],
    );
    let after = datetime!(2026-08-31 10:00 UTC);
    let queries = ["thsr", "bus", "metro", "tra"]
        .into_iter()
        .map(|id| {
            DepartureQuery::new(
                TransitPatternId::new(id),
                BoardingPointId::new("board"),
                after,
                1,
            )
        })
        .collect();

    let results = resolver.next_departures_batch(queries).await;

    let ids = results
        .into_iter()
        .map(|result| result.unwrap()[0].pattern_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        ["thsr", "bus", "metro", "tra"]
            .into_iter()
            .map(TransitPatternId::new)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        bus_calls.lock().unwrap().as_slice(),
        &[TransitPatternId::new("bus")]
    );
    assert_eq!(
        metro_calls.lock().unwrap().as_slice(),
        &[TransitPatternId::new("metro")]
    );
    assert_eq!(
        tra_calls.lock().unwrap().as_slice(),
        &[TransitPatternId::new("tra")]
    );
    assert_eq!(
        thsr_calls.lock().unwrap().as_slice(),
        &[TransitPatternId::new("thsr")]
    );
}

#[tokio::test]
async fn thsr_static_timetable_is_reliable() {
    let pattern_id = TransitPatternId::new("thsr-service");
    let board = BoardingPointId::new("board");
    let alight = BoardingPointId::new("alight");
    let network = Arc::new(
        TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            vec![TransitPattern::new(
                pattern_id.clone(),
                TransitRouteId::new("thsr"),
                None,
                "alight",
                vec![board.clone(), alight.clone()],
                SegmentProfile {
                    segments: vec![SegmentTravelTime {
                        seconds: 600,
                        source: SegmentTimeSource::Schedule,
                    }],
                },
            )],
            vec![TransitTrip::new(
                TransitTripId::new("thsr-trip"),
                pattern_id.clone(),
                ServiceCalendarId::new("monday"),
                vec![
                    ScheduledStopTime::new(36_000, 36_000),
                    ScheduledStopTime::new(36_600, 36_600),
                ],
            )],
        )
        .with_routes(routes())
        .with_calendars(vec![calendar()]),
    );
    let network = Arc::new(TransitRuntime::new(network));
    let source = Arc::new(ThsrDepartureSource::new(network.clone()));
    let resolver = DepartureResolver::new(network, vec![source as Arc<dyn TransitDepartureSource>]);

    let result = resolver
        .next_departures(DepartureQuery::new(
            pattern_id,
            board,
            datetime!(2026-08-31 09:50 +08:00),
            1,
        ))
        .await
        .unwrap();

    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Reliable);
}
