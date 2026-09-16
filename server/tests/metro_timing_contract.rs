use std::{fs, path::PathBuf, sync::Arc};

use time::macros::datetime;
use via_server::{
    data::build_from_raw,
    domain::{
        ids::{BoardingPointId, TransitServiceId},
        transit::{
            MetroStationTiming, MetroTiming, TransitMode, TransitNetwork,
            TransitNetworkValidationError, TransitService, TransitStop,
        },
        transit_timing::{
            DepartureQuery, DepartureResolver, ScheduleDepartureResolver, TimingBasis,
        },
    },
    providers::{CompositeDepartureResolver, MetroDepartureResolver},
};

fn temporary_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "via-metro-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ))
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

    let metro_departure = Arc::new(MetroDepartureResolver::new(network.clone()));
    let resolver = CompositeDepartureResolver::with_schedule_fallback(
        network.clone(),
        Arc::new(ScheduleDepartureResolver::new(network.clone())),
    )
    .with_adapter("metro_schedule", metro_departure.clone())
    .with_adapter("metro_frequency", metro_departure);
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

#[test]
fn metro_station_timetable_is_normalized_without_creating_scheduled_trips() {
    let input = temporary_path("raw");
    let output = temporary_path("artifact");
    fs::create_dir_all(input.join("metro")).unwrap();
    fs::write(
        input.join("metro/trtc-station.json"),
        r#"[
          {"StationUID":"TRTC-BL12","StationID":"BL12","StationName":{"Zh_tw":"台北車站"},"StationPosition":{"PositionLat":25.0478,"PositionLon":121.5170}},
          {"StationUID":"TRTC-R03","StationID":"R03","StationName":{"Zh_tw":"台北101/世貿"},"StationPosition":{"PositionLat":25.0339,"PositionLon":121.5645}}
        ]"#,
    )
    .unwrap();
    fs::write(
        input.join("metro/trtc-station-of-line.json"),
        r#"[
          {"LineID":"BL","LineName":{"Zh_tw":"板南線"},"Direction":0,"Stations":[{"Sequence":1,"StationID":"BL12"},{"Sequence":2,"StationID":"R03"}]}
        ]"#,
    )
    .unwrap();
    fs::write(
        input.join("metro/trtc-station-timetable.json"),
        r#"[
          {"LineID":"BL","Direction":0,"StationID":"BL12","Timetables":[{"Sequence":1,"TrainNo":"BL-0800","ArrivalTime":"08:00","DepartureTime":"08:00"},{"Sequence":2,"TrainNo":"BL-0805","ArrivalTime":"08:05","DepartureTime":"08:05"}]},
          {"LineID":"BL","Direction":1,"StationID":"R03","Timetables":[{"Sequence":1,"TrainNo":"BL-0900","ArrivalTime":"09:00","DepartureTime":"09:00"}]}
        ]"#,
    )
    .unwrap();
    fs::write(
        input.join("metro/trtc-s2s-travel-time.json"),
        r#"[
          {"LineID":"BL","Direction":0,"TravelTimes":[{"Sequence":1,"FromStationID":"BL12","ToStationID":"R03","RunTime":240,"StopTime":0}]}
        ]"#,
    )
    .unwrap();
    fs::write(
        input.join("metro/trtc-frequency.json"),
        r#"[
          {"LineID":"BL","Direction":0,"OperationTime":{"StartTime":"06:00","EndTime":"24:00"},"Headways":[{"StartTime":"08:00","EndTime":"08:10","MinHeadwayMins":2,"MaxHeadwayMins":3}]}
        ]"#,
    )
    .unwrap();

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
