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
        DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, TimingBasis,
        TimingReliability,
    },
};
use via_server::providers::timing::{
    DepartureDispatcher, ThsrDepartureSource, TransitDepartureSource,
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
async fn dispatcher_routes_by_mode_and_restores_original_order() {
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
    let dispatcher = DepartureDispatcher::new(
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

    let results = dispatcher.next_departures_batch(queries).await;

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
    let dispatcher =
        DepartureDispatcher::new(network, vec![source as Arc<dyn TransitDepartureSource>]);

    let result = dispatcher
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
