use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use time::Weekday;
use via_server::{
    data::{DataScope, build_from_raw},
    domain::{
        ids::{BoardingPointId, TransitServiceId},
        transit::TransitMode,
        transit_timing::{DepartureQuery, DepartureResolver},
    },
    providers::MetroDepartureResolver,
};

fn temporary_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "via-current-tdx-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

#[test]
fn current_tdx_payloads_normalize_all_static_operator_families() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tdx/raw");
    let output = temporary_path("artifact");

    let result = build_from_raw(
        &input,
        &output,
        Some(DataScope::TaipeiNewTaipei),
        Some("2026-09-15".into()),
    )
    .unwrap();

    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.transit_mode == TransitMode::Bus)
    );
    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.transit_mode == TransitMode::Metro)
    );
    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.transit_mode == TransitMode::Rail)
    );
    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.transit_mode == TransitMode::HighSpeedRail)
    );
    assert_eq!(result.network.bike_share_stations.len(), 2);
    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.id.as_str() == "TPE-307-0::direction=0")
    );
    assert!(
        result
            .network
            .services
            .iter()
            .any(|service| service.id.as_str() == "NWT-242-0::direction=0")
    );
    assert!(result.network.services.iter().any(|service| {
        service.transit_mode == TransitMode::Bus
            && service.realtime_scope.as_deref() == Some("Taipei")
    }));
    assert!(result.network.services.iter().any(|service| {
        service.transit_mode == TransitMode::Bus
            && service.realtime_scope.as_deref() == Some("NewTaipei")
    }));
    assert!(result.network.services.iter().all(|service| {
        service.transit_mode == TransitMode::Bus || service.realtime_scope.is_none()
    }));
    assert!(
        result
            .network
            .metro_timings
            .iter()
            .any(|timing| { timing.inter_station_travel_seconds == vec![240] })
    );
    assert!(result.network.metro_timings.iter().any(|timing| {
        timing
            .station_timings
            .iter()
            .any(|station| station.departures_seconds == vec![28_800, 29_100])
    }));
    assert!(result.network.trips.iter().any(|trip| {
        trip.service_id.as_str() == "tra:100"
            && trip.active_weekdays
                == vec![
                    Weekday::Monday,
                    Weekday::Tuesday,
                    Weekday::Wednesday,
                    Weekday::Thursday,
                    Weekday::Friday,
                ]
    }));
    assert!(
        result
            .network
            .trips
            .iter()
            .any(|trip| trip.service_id.as_str() == "thsr:100")
    );
    result.network.validate().unwrap();
}

#[tokio::test]
async fn real_tdx_red_line_has_boardable_directional_services() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/dev/taipei/raw/2026-09-15");
    let output = temporary_path("real-red-line-artifact");
    let result = build_from_raw(&input, &output, None, Some("2026-09-15".into())).unwrap();
    let network = Arc::new(result.network);
    let realtime_scopes = network
        .services
        .iter()
        .filter(|service| service.transit_mode == TransitMode::Bus)
        .filter_map(|service| service.realtime_scope.as_deref())
        .collect::<BTreeSet<_>>();
    assert_eq!(realtime_scopes, BTreeSet::from(["NewTaipei", "Taipei"]));
    let forward_id = TransitServiceId::new("metro:R::direction=0");
    let reverse_id = TransitServiceId::new("metro:R::direction=1");
    let taipei_main = BoardingPointId::new("TRTC-R10");
    let taipei_101 = BoardingPointId::new("TRTC-R03");

    let forward = network.service(&forward_id).unwrap();
    let reverse = network.service(&reverse_id).unwrap();
    assert_eq!(
        forward.pattern.first(),
        Some(&BoardingPointId::new("TRTC-R02"))
    );
    assert_eq!(
        forward.pattern.last(),
        Some(&BoardingPointId::new("TRTC-R28"))
    );
    assert_eq!(
        reverse.pattern.first(),
        Some(&BoardingPointId::new("TRTC-R28"))
    );
    assert_eq!(
        reverse.pattern.last(),
        Some(&BoardingPointId::new("TRTC-R02"))
    );
    assert!(forward.pattern.contains(&taipei_main));
    assert!(forward.pattern.contains(&taipei_101));
    assert!(reverse.pattern.contains(&taipei_main));
    assert!(reverse.pattern.contains(&taipei_101));
    assert_eq!(
        network
            .services
            .iter()
            .filter(|service| service.transit_mode == TransitMode::Metro)
            .count(),
        network.metro_timings.len()
    );

    for service_id in [&forward_id, &reverse_id] {
        let service = network.service(service_id).unwrap();
        let timing = network
            .metro_timings
            .iter()
            .find(|timing| &timing.service_id == service_id)
            .unwrap();
        assert_eq!(
            timing.inter_station_travel_seconds.len(),
            service.pattern.len() - 1
        );
        assert_eq!(timing.pattern, service.pattern);
        assert!(
            timing
                .inter_station_travel_seconds
                .iter()
                .all(|seconds| *seconds > 0)
        );
    }

    let resolver = MetroDepartureResolver::new(network.clone());
    let after = time::macros::datetime!(2026-09-15 08:00 +08:00);
    let forward_trips = resolver
        .next_departures(DepartureQuery::new(
            forward_id,
            taipei_101.clone(),
            after,
            1,
        ))
        .await
        .unwrap();
    let reverse_trips = resolver
        .next_departures(DepartureQuery::new(
            reverse_id,
            taipei_main.clone(),
            after,
            1,
        ))
        .await
        .unwrap();
    assert!(forward_trips.iter().any(|trip| {
        let board = trip
            .stop_times
            .iter()
            .position(|stop| stop.stop_id == taipei_101);
        let alight = trip
            .stop_times
            .iter()
            .position(|stop| stop.stop_id == taipei_main);
        board.is_some_and(|board| alight.is_some_and(|alight| board < alight))
    }));
    assert!(reverse_trips.iter().any(|trip| {
        let board = trip
            .stop_times
            .iter()
            .position(|stop| stop.stop_id == taipei_main);
        let alight = trip
            .stop_times
            .iter()
            .position(|stop| stop.stop_id == taipei_101);
        board.is_some_and(|board| alight.is_some_and(|alight| board < alight))
    }));
}
