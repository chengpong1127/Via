use std::path::PathBuf;

use time::Weekday;
use via_server::{data::build_from_raw, domain::transit::TransitMode};

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

    let result = build_from_raw(&input, &output, None, Some("2026-09-15".into())).unwrap();

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
