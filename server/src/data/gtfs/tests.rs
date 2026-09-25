use std::{fs::File, io::Write, path::PathBuf};

use time::macros::date;
use zip::{ZipWriter, write::SimpleFileOptions};

use super::{
    build_from_gtfs_archive_with_street,
    identity::tdx_stop_id,
    reader::{parse_gtfs_date, parse_gtfs_time},
    routes::parse_route_mode,
    segment_profile::{distribute_runtime, is_chronological_times},
};
use crate::domain::transit::{ScheduledStopTime, TransitMode};
use crate::street::{Coordinate, FakeStreetRouter, Metres, Seconds, StreetTravelMode};

fn temporary_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "via-gtfs-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn write_gtfs_fixture(path: &PathBuf) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default();
    for (name, body) in [
        (
            "stops.txt",
            "\u{feff}stop_id,stop_name,stop_lat,stop_lon,location_type\nTPE001,Main Bus,25.0478,121.5170,0\nTPE002,Mid Bus,25.0400,121.5300,0\nTPE003,End Bus,25.0339,121.5645,0\nTRTC_BL12,Taipei Main,25.0478,121.5170,0\nTRTC_BL13,Blue Mid,25.0430,121.5350,0\nTRTC_BL14,Blue End,25.0400,121.5500,0\n",
        ),
        (
            "routes.txt",
            "\u{feff}route_id,agency_id,route_short_name,route_long_name,route_type\nTHB123_0,THB,1,Bus line,3\nTRTC_BL_BL-1_0,TRTC,BL,Blue line,1\n",
        ),
        (
            "trips.txt",
            "\u{feff}route_id,service_id,trip_id,direction_id,trip_headsign,shape_id\nTHB123_0,weekdays,bus-1,0,Bus end,bus-shape\nTHB123_0,weekdays,bus-2,0,Bus end,bus-shape\nTHB123_0,weekdays,bus-branch,0,Bus end,bus-shape\nTRTC_BL_BL-1_0,weekdays,metro-frequency,0,Blue end,\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence,shape_dist_traveled\nbus-1,,08:00:00,TPE001,1,0\nbus-1,,,TPE002,2,2\nbus-1,08:30:00,,TPE003,3,10\nbus-2,,09:00:00,TPE001,1,0\nbus-2,,,TPE002,2,2\nbus-2,09:40:00,,TPE003,3,10\nbus-branch,,08:10:00,TPE001,1,0\nbus-branch,08:35:00,,TPE003,2,10\nmetro-frequency,08:00:00,08:00:00,TRTC_BL12,1,\nmetro-frequency,08:04:00,08:04:00,TRTC_BL13,2,\nmetro-frequency,08:07:00,08:07:00,TRTC_BL14,3,\n",
        ),
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nweekdays,1,1,1,1,1,1,1,20260901,20261231\n",
        ),
        (
            "calendar_dates.txt",
            "service_id,date,exception_type\nweekdays,20260923,2\n",
        ),
        (
            "frequencies.txt",
            "trip_id,start_time,end_time,headway_secs,exact_times\nmetro-frequency,09:00:00,10:00:00,300,1\n",
        ),
        (
            "shapes.txt",
            "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence\nbus-shape,25.0478,121.5170,1\nbus-shape,25.0400,121.5300,2\nbus-shape,25.0339,121.5645,3\n",
        ),
        (
            "transfers.txt",
            "from_stop_id,to_stop_id,transfer_type,min_transfer_time\nTRTC_BL13,TPE002,2,240\n",
        ),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn parses_gtfs_extended_times_and_dates() {
    assert_eq!(parse_gtfs_time("25:10:05"), Some(90_605));
    assert_eq!(parse_gtfs_time("25:61:00"), None);
    assert_eq!(
        parse_gtfs_date("20260923").unwrap().to_string(),
        "2026-09-23"
    );
}

#[test]
fn maps_gtfs_modes_and_canonical_tdx_stop_ids() {
    assert_eq!(parse_route_mode("3", "THB123_0"), Some(TransitMode::Bus));
    assert_eq!(
        parse_route_mode("1", "TRTC_BL_BL-1_0"),
        Some(TransitMode::Metro)
    );
    assert_eq!(
        parse_route_mode("2", "THSR_0108_1"),
        Some(TransitMode::HighSpeedRail)
    );
    assert_eq!(tdx_stop_id("TRTC_BL12"), "TRTC-BL12");
    assert_eq!(tdx_stop_id("THB300516"), "THB300516");
}

#[test]
fn allocates_bus_runtime_by_distance_without_losing_total_runtime() {
    let durations = distribute_runtime(101, &[1.0, 2.0, 3.0]).unwrap();
    assert_eq!(durations.iter().sum::<u32>(), 101);
    assert_eq!(durations, vec![17, 34, 50]);
}

#[test]
fn complete_exact_trip_times_must_be_chronological() {
    assert!(is_chronological_times(&[
        ScheduledStopTime::new(22_800, 22_800),
        ScheduledStopTime::new(23_040, 23_040),
    ]));
    assert!(!is_chronological_times(&[
        ScheduledStopTime::new(23_040, 23_040),
        ScheduledStopTime::new(60, 60),
    ]));
}

#[tokio::test]
async fn builds_network_timing_and_via_transfers_ignoring_gtfs_transfers() {
    let input = temporary_path("input.zip");
    let output = temporary_path("output");
    write_gtfs_fixture(&input);

    let first = Coordinate::try_new(25.0478, 121.5170).unwrap();
    let street = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Walking,
            first,
            first,
            Seconds(75),
            Metres(50),
        )
        .build();
    let built = build_from_gtfs_archive_with_street(&input, &output, &street)
        .await
        .unwrap();
    let network = &built.build.network;
    assert_eq!(built.diagnostics.stops_loaded, 6);
    assert_eq!(built.diagnostics.trips_loaded, 4);
    assert_eq!(built.diagnostics.frequencies_loaded, 1);
    assert!(built.diagnostics.static_transfers.is_some());
    assert_eq!(built.diagnostics.invalid_timing_trips, 0);
    assert_eq!(built.diagnostics.exact_static_timing_patterns, 1);
    assert_eq!(built.diagnostics.derived_bus_timing_patterns, 2);
    assert_eq!(built.diagnostics.geometric_fallback_patterns, 0);
    assert_eq!(built.diagnostics.unresolved_bus_realtime_scope_patterns, 0);
    assert_eq!(built.diagnostics.gtfs_pattern_ids_without_tdx_mapping, 0);
    assert!(
        network.trips.is_empty(),
        "sparse Bus stop times are not exact trips"
    );
    assert_eq!(network.frequencies.len(), 1);
    let bus_patterns = network
        .patterns
        .iter()
        .filter(|pattern| network.route_of(pattern).mode == TransitMode::Bus)
        .collect::<Vec<_>>();
    assert_eq!(
        bus_patterns.len(),
        2,
        "two identical trip stop sequences share one Pattern; a branch has its own"
    );
    let bus = bus_patterns
        .iter()
        .find(|pattern| pattern.stops.len() == 3)
        .unwrap();
    assert_eq!(bus.route_id, bus_patterns[1].route_id);
    assert_eq!(network.route_of(bus).label, "1");
    assert_eq!(
        network
            .routes
            .iter()
            .find(|route| route.mode == TransitMode::Metro)
            .unwrap()
            .label,
        "BL"
    );
    assert_eq!(
        bus.static_profile
            .segments
            .iter()
            .map(|segment| segment.seconds)
            .collect::<Vec<_>>(),
        vec![420, 1680]
    );
    assert!(
        bus.static_profile
            .segments
            .iter()
            .all(|segment| segment.source == crate::domain::transit::SegmentTimeSource::Derived)
    );
    let persisted =
        crate::data::load_network_artifact(&output.join("transit-network.json")).unwrap();
    assert_eq!(
        persisted
            .patterns
            .iter()
            .find(|pattern| pattern.id == bus.id)
            .unwrap()
            .static_profile,
        bus.static_profile
    );
    assert_eq!(built.build.provider_bindings.bus_patterns.len(), 2);
    assert_eq!(built.build.provider_bindings.metro_patterns.len(), 1);
    assert!(
        network
            .transfer_edges
            .iter()
            .any(|edge| edge.duration_seconds == 75)
    );
    assert!(
        !network
            .transfer_edges
            .iter()
            .any(|edge| edge.duration_seconds == 240)
    );
    let calendar = network
        .calendar(&network.frequencies[0].calendar_id)
        .unwrap();
    assert!(calendar.runs_on(date!(2026 - 09 - 22)));
    assert!(!calendar.runs_on(date!(2026 - 09 - 23)));
    assert!(calendar.runs_on(date!(2026 - 09 - 24)));
    assert!(
        network
            .pattern(&crate::domain::ids::TransitPatternId::new(
                "pattern:metro:BL::direction=0::variant=0"
            ))
            .is_some()
    );
    assert!(
        network
            .pattern(&crate::domain::ids::TransitPatternId::new(
                "pattern:THB123::direction=0::variant=0"
            ))
            .is_some()
    );

    std::fs::remove_file(input).unwrap();
    std::fs::remove_dir_all(output).unwrap();
}
