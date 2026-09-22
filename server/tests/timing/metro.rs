use std::{path::PathBuf, sync::Arc};

use time::macros::datetime;
use via_server::{
    data::build_from_raw,
    domain::{
        ids::{BoardingPointId, TransitServiceId},
        transit::{
            MetroFrequency, MetroStationTiming, MetroTiming, TransitMode, TransitNetwork,
            TransitNetworkValidationError, TransitService, TransitStop,
        },
        transit_timing::{DepartureQuery, TimingBasis, TimingReliability},
    },
    providers::timing::{DepartureDispatcher, MetroDepartureSource, TransitDepartureSource},
};

fn temporary_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "via-metro-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/test-data/metro-timing")
}

#[tokio::test]
async fn metro_static_timing_materializes_taipei_main_to_taipei_101() {
    let main_station = BoardingPointId::new("TRTC-BL12");
    let taipei_101 = BoardingPointId::new("TRTC-R03");
    let service_id = TransitServiceId::new("metro:example");
    let network = Arc::new(
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![
                TransitStop::new(
                    main_station.clone(),
                    "Taipei Main Station",
                    via_server::api::RequestCoordinate {
                        latitude: 25.0478,
                        longitude: 121.5170,
                    },
                ),
                TransitStop::new(
                    taipei_101.clone(),
                    "Taipei 101/World Trade Center",
                    via_server::api::RequestCoordinate {
                        latitude: 25.0339,
                        longitude: 121.5645,
                    },
                ),
            ],
            vec![
                TransitService::new(
                    service_id.clone(),
                    TransitMode::Metro,
                    "Example Metro",
                    "Taipei 101/World Trade Center",
                    vec![main_station.clone(), taipei_101.clone()],
                )
                .with_timing_source("metro_schedule"),
            ],
            Vec::new(),
        )
        .with_metro_timings(vec![MetroTiming::new(
            service_id.clone(),
            vec![MetroStationTiming {
                stop_id: main_station.clone(),
                departures_seconds: vec![28_800, 29_100],
                frequencies: Vec::new(),
            }],
            vec![240],
        )])
        .with_revisions("static", "schedule"),
    );
    network.validate().unwrap();

    let metro = Arc::new(MetroDepartureSource::new(network.clone()));
    let resolver = DepartureDispatcher::new(
        network.clone(),
        vec![metro as Arc<dyn TransitDepartureSource>],
    );
    let departures = resolver
        .next_departures(DepartureQuery::new(
            service_id,
            main_station,
            datetime!(2026-09-15 07:55 +08:00),
            2,
        ))
        .await
        .unwrap();

    assert_eq!(departures.len(), 2);
    assert!(matches!(departures[0].timing_basis, TimingBasis::Schedule));
    assert_eq!(departures[0].stop_times.len(), 2);
    assert_eq!(
        departures[0].stop_times[0].departure_at,
        datetime!(2026-09-15 08:00 +08:00)
    );
    assert_eq!(
        departures[0].stop_times[1].arrival_at,
        datetime!(2026-09-15 08:04 +08:00)
    );
    assert!(departures[0].stop_times[1].arrival_at > departures[0].stop_times[0].departure_at);
    assert_eq!(
        departures[1].stop_times[0].departure_at,
        datetime!(2026-09-15 08:05 +08:00)
    );
}

#[tokio::test]
async fn metro_frequency_timing_remains_static_fallback() {
    let first = BoardingPointId::new("TRTC-BL12");
    let second = BoardingPointId::new("TRTC-BL23");
    let service_id = TransitServiceId::new("metro:BL::direction=0");
    let network = Arc::new(
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![
                TransitStop::new(
                    first.clone(),
                    "Taipei Main Station",
                    via_server::api::RequestCoordinate {
                        latitude: 25.0478,
                        longitude: 121.5170,
                    },
                ),
                TransitStop::new(
                    second.clone(),
                    "Longshan Temple",
                    via_server::api::RequestCoordinate {
                        latitude: 25.0354,
                        longitude: 121.4998,
                    },
                ),
            ],
            vec![TransitService::new(
                service_id.clone(),
                TransitMode::Metro,
                "板南線",
                "南港展覽館",
                vec![first.clone(), second],
            )],
            Vec::new(),
        )
        .with_metro_timings(vec![MetroTiming::new(
            service_id,
            vec![MetroStationTiming {
                stop_id: first,
                departures_seconds: Vec::new(),
                frequencies: vec![MetroFrequency {
                    start_seconds: 28_800,
                    end_seconds: 29_400,
                    headway_seconds: 300,
                }],
            }],
            vec![240],
        )])
        .with_revisions("static", "schedule"),
    );

    let metro = Arc::new(MetroDepartureSource::new(network.clone()));
    let resolver =
        DepartureDispatcher::new(network, vec![metro as Arc<dyn TransitDepartureSource>]);
    let departures = resolver
        .next_departures(DepartureQuery::new(
            TransitServiceId::new("metro:BL::direction=0"),
            BoardingPointId::new("TRTC-BL12"),
            datetime!(2026-09-15 07:55 +08:00),
            2,
        ))
        .await
        .unwrap();

    assert_eq!(departures.len(), 2);
    assert!(
        departures
            .iter()
            .all(|departure| departure.timing_basis == TimingBasis::FrequencyEstimate)
    );
    assert!(
        departures
            .iter()
            .all(|departure| departure.timing_reliability == TimingReliability::Fallback)
    );
}

#[tokio::test]
async fn metro_static_snapshot_window_crosses_the_service_day_boundary() {
    let first = BoardingPointId::new("TRTC-A");
    let second = BoardingPointId::new("TRTC-B");
    let service_id = TransitServiceId::new("metro:boundary");
    let network = Arc::new(
        TransitNetwork::new(
            time::macros::time!(4:00),
            vec![
                TransitStop::new(
                    first.clone(),
                    "A",
                    via_server::api::RequestCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                ),
                TransitStop::new(
                    second.clone(),
                    "B",
                    via_server::api::RequestCoordinate {
                        latitude: 25.1,
                        longitude: 121.6,
                    },
                ),
            ],
            vec![TransitService::new(
                service_id.clone(),
                TransitMode::Metro,
                "Boundary",
                "B",
                vec![first.clone(), second],
            )],
            Vec::new(),
        )
        .with_metro_timings(vec![MetroTiming::new(
            service_id.clone(),
            vec![MetroStationTiming {
                stop_id: first.clone(),
                departures_seconds: vec![27 * 3600 + 55 * 60, 4 * 3600 + 10 * 60],
                frequencies: Vec::new(),
            }],
            vec![240],
        )])
        .with_revisions("static", "schedule"),
    );
    let metro = Arc::new(MetroDepartureSource::new(network.clone()));
    let resolver =
        DepartureDispatcher::new(network, vec![metro as Arc<dyn TransitDepartureSource>]);

    let departures = resolver
        .next_departures(
            DepartureQuery::at_position(
                service_id,
                first,
                0,
                datetime!(2026-08-31 03:50 +08:00),
                10,
            )
            .until(datetime!(2026-08-31 04:30 +08:00)),
        )
        .await
        .unwrap();

    assert_eq!(departures.len(), 2);
    assert_eq!(
        departures[0].stop_times[0].departure_at,
        datetime!(2026-08-31 03:55 +08:00)
    );
    assert_eq!(
        departures[1].stop_times[0].departure_at,
        datetime!(2026-08-31 04:10 +08:00)
    );
}

#[test]
fn metro_station_timetable_is_normalized_without_creating_scheduled_trips() {
    let input = fixture_path();
    let output = temporary_path("artifact");

    let result = build_from_raw(&input, &output, None, None).unwrap();
    assert!(result.network.trips.is_empty());
    assert_eq!(result.network.metro_timings.len(), 2);
    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.id.as_str() == "metro:BL::direction=0")
    );
    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.id.as_str() == "metro:BL::direction=1")
    );
    let forward_timing = result
        .network
        .metro_timings
        .iter()
        .find(|timing| timing.service_id.as_str() == "metro:BL::direction=0")
        .unwrap();
    let reverse_timing = result
        .network
        .metro_timings
        .iter()
        .find(|timing| timing.service_id.as_str() == "metro:BL::direction=1")
        .unwrap();
    assert_eq!(forward_timing.inter_station_travel_seconds, vec![240]);
    assert_eq!(
        forward_timing.station_timings[0].departures_seconds,
        vec![28_800, 29_100]
    );
    assert!(
        forward_timing
            .station_timings
            .iter()
            .all(|station| !station.departures_seconds.contains(&32_400))
    );
    assert_eq!(
        forward_timing.station_timings[0].frequencies[0].headway_seconds,
        120
    );
    assert_eq!(reverse_timing.inter_station_travel_seconds, vec![240]);
    assert_eq!(
        reverse_timing
            .station_timings
            .iter()
            .find(|station| station.stop_id.as_str() == "TRTC-R03")
            .unwrap()
            .departures_seconds,
        vec![32_400]
    );
}

#[test]
fn metro_timing_rejects_a_pattern_from_the_opposite_direction() {
    let first = BoardingPointId::new("TRTC-R03");
    let second = BoardingPointId::new("TRTC-R10");
    let service_id = TransitServiceId::new("metro:R::direction=0");
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![
            TransitStop::new(
                first.clone(),
                "Taipei 101/World Trade Center",
                via_server::api::RequestCoordinate {
                    latitude: 25.0339,
                    longitude: 121.5645,
                },
            ),
            TransitStop::new(
                second.clone(),
                "Taipei Main Station",
                via_server::api::RequestCoordinate {
                    latitude: 25.0478,
                    longitude: 121.5170,
                },
            ),
        ],
        vec![TransitService::new(
            service_id.clone(),
            TransitMode::Metro,
            "Red Line",
            "Taipei Main Station",
            vec![first.clone(), second.clone()],
        )],
        Vec::new(),
    )
    .with_metro_timings(vec![
        MetroTiming::new(
            service_id.clone(),
            vec![MetroStationTiming {
                stop_id: first,
                departures_seconds: vec![28_800],
                frequencies: Vec::new(),
            }],
            vec![240],
        )
        .with_pattern(vec![second, BoardingPointId::new("TRTC-R03")]),
    ])
    .with_revisions("static", "schedule");

    assert!(matches!(
        network.validate(),
        Err(TransitNetworkValidationError::InvalidMetroTimingShape(id)) if id == service_id
    ));
}
