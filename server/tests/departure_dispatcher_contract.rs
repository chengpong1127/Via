use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::{Time, Weekday, macros::datetime};
use via_server::domain::{
    ids::{BoardingPointId, EffectiveTripId, ScheduledTripId, TransitServiceId},
    transit::{ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService},
    transit_timing::{
        DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
        TimingBasis, TimingReliability,
    },
};
use via_server::providers::timing::{
    DepartureDispatcher, ThsrDepartureSource, TransitDepartureSource,
};

#[derive(Clone)]
struct RecordingSource {
    mode: TransitMode,
    calls: Arc<Mutex<Vec<TransitServiceId>>>,
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
        self.calls.lock().unwrap().push(query.service_id.clone());
        Ok(vec![EffectiveTrip {
            id: EffectiveTripId::new(format!("fixture:{}", query.service_id.as_str())),
            service_id: query.service_id,
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

fn service(id: &str, mode: TransitMode) -> TransitService {
    TransitService::new(
        TransitServiceId::new(id),
        mode,
        id,
        "alight",
        vec![
            BoardingPointId::new("board"),
            BoardingPointId::new("alight"),
        ],
    )
}

#[tokio::test]
async fn dispatcher_routes_by_mode_and_restores_original_order() {
    let services = vec![
        service("bus", TransitMode::Bus),
        service("metro", TransitMode::Metro),
        service("tra", TransitMode::Rail),
        service("thsr", TransitMode::HighSpeedRail),
    ];
    let network = Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        services,
        Vec::new(),
    ));
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
                TransitServiceId::new(id),
                BoardingPointId::new("board"),
                after,
                1,
            )
        })
        .collect();

    let results = dispatcher.next_departures_batch(queries).await;

    let ids = results
        .into_iter()
        .map(|result| result.unwrap()[0].service_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        ["thsr", "bus", "metro", "tra"]
            .into_iter()
            .map(TransitServiceId::new)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        bus_calls.lock().unwrap().as_slice(),
        &[TransitServiceId::new("bus")]
    );
    assert_eq!(
        metro_calls.lock().unwrap().as_slice(),
        &[TransitServiceId::new("metro")]
    );
    assert_eq!(
        tra_calls.lock().unwrap().as_slice(),
        &[TransitServiceId::new("tra")]
    );
    assert_eq!(
        thsr_calls.lock().unwrap().as_slice(),
        &[TransitServiceId::new("thsr")]
    );
}

#[tokio::test]
async fn thsr_static_timetable_is_reliable() {
    let service_id = TransitServiceId::new("thsr-service");
    let board = BoardingPointId::new("board");
    let alight = BoardingPointId::new("alight");
    let network = Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        vec![TransitService::new(
            service_id.clone(),
            TransitMode::HighSpeedRail,
            "THSR",
            "alight",
            vec![board.clone(), alight.clone()],
        )],
        vec![ScheduledTrip::weekdays(
            ScheduledTripId::new("thsr-trip"),
            service_id.clone(),
            vec![Weekday::Monday],
            vec![
                ScheduledStopTime::new(36_000, 36_000),
                ScheduledStopTime::new(36_600, 36_600),
            ],
        )],
    ));
    let source = Arc::new(ThsrDepartureSource::new(network.clone()));
    let dispatcher =
        DepartureDispatcher::new(network, vec![source as Arc<dyn TransitDepartureSource>]);

    let result = dispatcher
        .next_departures(DepartureQuery::new(
            service_id,
            board,
            datetime!(2026-08-31 09:50 +08:00),
            1,
        ))
        .await
        .unwrap();

    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
    assert_eq!(result[0].timing_reliability, TimingReliability::Reliable);
}
