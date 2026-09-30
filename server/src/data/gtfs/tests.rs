use std::{collections::BTreeMap, fs::File, io::Write, path::Path, path::PathBuf};

use time::macros::date;
use zip::{ZipWriter, write::SimpleFileOptions};

use super::{
    bindings::tdx_stop_id,
    normalize::{has_unique_stops, is_chronological_times, route_headsign_fallback, via_mode},
    segment_profile::distribute_runtime,
};
use crate::data::{
    build_from_fetch_snapshot,
    fetch::{FetchManifest, FetchSource},
};
use crate::domain::{
    transit::{ScheduledStopTime, TransitMode},
    transit_runtime::TransitRuntime,
};
use crate::street::{Coordinate, FakeStreetRouter, Metres, Seconds, StreetTravelMode};
use gtfs_structures::RouteType;

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
            "\u{feff}route_id,service_id,trip_id,direction_id,trip_headsign,shape_id\nTHB123_0,weekdays,bus-1,eastbound,Bus end,bus-shape\nTHB123_0,weekdays,bus-2,0,Bus end,bus-shape\nTHB123_0,weekdays,bus-branch,0,Bus end,bus-shape\nTRTC_BL_BL-1_0,weekdays,metro-frequency,0,Blue end,\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence,shape_dist_traveled\nbus-1,,08:00:00,TPE001,1,0\nbus-1,,,TPE002,2,2\nbus-1,08:30:00,,TPE003,3,10\nbus-2,,09:00:00,TPE001,1,0\nbus-2,,,TPE002,2,2\nbus-2,09:40:00,,TPE003,3,10\nbus-branch,,25:10:05,TPE001,1,0\nbus-branch,25:35:05,,TPE003,2,10\nmetro-frequency,08:00:00,08:00:00,TRTC_BL12,1,\nmetro-frequency,08:04:00,08:04:00,TRTC_BL13,2,\nmetro-frequency,08:07:00,08:07:00,TRTC_BL14,3,\ntime-probe,,10:30,TPE001,1,\ntime-probe,10:30:00,,TPE002,2,\ntime-probe,,25:30,TPE001,3,\ntime-probe,25:30:00,,TPE002,4,\n",
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

fn write_fetch_snapshot(gtfs: &PathBuf, snapshot: &PathBuf) {
    std::fs::create_dir_all(snapshot).unwrap();
    let gtfs_path = snapshot.join("gtfs.zip");
    std::fs::copy(gtfs, &gtfs_path).unwrap();
    let taipei_path = snapshot.join("bike-stations-taipei.json");
    let new_taipei_path = snapshot.join("bike-stations-new-taipei.json");
    std::fs::write(&taipei_path, "[]").unwrap();
    std::fs::write(&new_taipei_path, "[]").unwrap();
    let source = |path: &PathBuf, source_id: &str| {
        let bytes = std::fs::read(path).unwrap();
        FetchSource {
            source_id: source_id.to_owned(),
            url: None,
            sha256: crate::data::sha256_hex(&bytes),
            byte_size: bytes.len() as u64,
        }
    };
    let manifest = FetchManifest {
        manifest_version: 1,
        fetched_at: "2026-09-28T00:00:00Z".to_owned(),
        sources: BTreeMap::from([
            ("gtfs".to_owned(), source(&gtfs_path, "local-gtfs-file")),
            (
                "bike-stations-taipei".to_owned(),
                source(&taipei_path, "tdx-bike-station-city"),
            ),
            (
                "bike-stations-new-taipei".to_owned(),
                source(&new_taipei_path, "tdx-bike-station-city"),
            ),
        ]),
    };
    std::fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn refresh_fetch_source(snapshot: &Path, source_name: &str) {
    let manifest_path = snapshot.join("manifest.json");
    let mut manifest =
        serde_json::from_slice::<FetchManifest>(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let source = manifest.sources.get_mut(source_name).unwrap();
    let source_path = snapshot.join(crate::data::fetch::source_file_name(source_name).unwrap());
    let bytes = std::fs::read(source_path).unwrap();
    source.sha256 = crate::data::sha256_hex(&bytes);
    source.byte_size = bytes.len() as u64;
    std::fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

fn write_tables(path: &Path, tables: Vec<(&str, String)>) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default();
    for (name, body) in tables {
        zip.start_file(name, options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn write_coverage_fixture(path: &Path, unreferenced_stop_count: usize) {
    let mut stops = String::from(
        "stop_id,stop_name,stop_lat,stop_lon,location_type\nTRTC_BL12,Metro A,25.0478,121.5170,0\nTRTC_BL13,Metro B,25.0400,121.5300,0\n",
    );
    for index in 0..unreferenced_stop_count {
        stops.push_str(&format!("UNUSED_{index:03},Unused stop,25.0,121.5,0\n"));
    }
    write_tables(
        path,
        vec![
            ("stops.txt", stops),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_long_name,route_type\nTHB111_0,THB,111,No pattern,3\nTHB222_0,THB,222,Unknown stops,3\nTRTC_BL_BL-1_0,TRTC,BL,Blue line,1\n".to_owned(),
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,direction_id,trip_headsign\nTHB222_0,ghost,bus-no-pattern,0,Nowhere\nTRTC_BL_BL-1_0,ghost,metro-no-calendar,0,Metro B\n".to_owned(),
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nbus-no-pattern,08:00:00,08:00:00,UNKNOWN_A,1\nbus-no-pattern,08:05:00,08:05:00,UNKNOWN_B,2\nmetro-no-calendar,09:00:00,09:00:00,TRTC_BL12,1\nmetro-no-calendar,09:04:00,09:04:00,TRTC_BL13,2\n".to_owned(),
            ),
        ],
    );
}

fn write_no_service_fixture(path: &Path) {
    write_tables(
        path,
        vec![
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon,location_type\nTPE001,Main,25.0478,121.5170,0\nTPE002,End,25.0400,121.5300,0\n".to_owned(),
            ),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_long_name,route_type\nTHB111_0,THB,111,No pattern,3\n".to_owned(),
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,direction_id\n".to_owned(),
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n".to_owned(),
            ),
        ],
    );
}

#[test]
fn maps_gtfs_modes_and_canonical_tdx_stop_ids() {
    assert_eq!(via_mode(RouteType::Bus, "THB123_0"), Some(TransitMode::Bus));
    assert_eq!(
        via_mode(RouteType::Subway, "TRTC_BL_BL-1_0"),
        Some(TransitMode::Metro)
    );
    assert_eq!(
        via_mode(RouteType::Rail, "THSR_0108_1"),
        Some(TransitMode::HighSpeedRail)
    );
    assert_eq!(tdx_stop_id("TRTC_BL12"), "TRTC-BL12");
    assert_eq!(tdx_stop_id("THB300516"), "THB300516");
}

#[test]
fn raw_gtfs_keeps_sparse_times_and_extended_service_day_times() {
    let input = temporary_path("raw-parser.zip");
    write_gtfs_fixture(&input);

    let raw = super::archive::read(&input).unwrap();
    let stop_times = raw.stop_times.unwrap();
    let sparse = stop_times
        .iter()
        .find(|time| time.trip_id == "bus-1" && time.stop_id == "TPE002")
        .unwrap();
    assert_eq!(sparse.arrival_time, None);
    assert_eq!(sparse.departure_time, None);
    let extended = stop_times
        .iter()
        .find(|time| time.trip_id == "bus-branch" && time.stop_id == "TPE001")
        .unwrap();
    assert_eq!(extended.departure_time, Some(90_605));

    let parsed_times = stop_times
        .iter()
        .filter(|time| time.trip_id == "time-probe")
        .collect::<Vec<_>>();
    assert_eq!(parsed_times[0].departure_time, Some(37_800));
    assert_eq!(parsed_times[1].arrival_time, Some(37_800));
    assert_eq!(parsed_times[2].departure_time, Some(91_800));
    assert_eq!(parsed_times[3].arrival_time, Some(91_800));
    let ignored_direction = raw
        .trips
        .unwrap()
        .into_iter()
        .find(|trip| trip.id == "bus-1")
        .unwrap();
    assert_eq!(ignored_direction.direction_id, None);

    std::fs::remove_file(input).unwrap();
}

#[test]
fn rejects_duplicate_gtfs_table_basenames_in_separate_directories() {
    let input = temporary_path("duplicate-table-basename.zip");
    let mut zip = ZipWriter::new(File::create(&input).unwrap());
    for name in ["a/routes.txt", "b/routes.txt"] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(b"route_id,route_type\nroute,3\n").unwrap();
    }
    zip.finish().unwrap();

    let error = match super::archive::read(&input) {
        Ok(_) => panic!("duplicate routes.txt entries should be rejected"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        crate::data::DataError::InvalidGtfsArchive { detail, .. }
            if detail.contains("duplicate GTFS table routes.txt")
    ));
    std::fs::remove_file(input).unwrap();
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

#[test]
fn repeated_stops_are_rejected_and_headsign_falls_back_to_route_label() {
    assert!(has_unique_stops(&[1, 2, 3]));
    assert!(!has_unique_stops(&[1, 2, 1]));

    let route = gtfs_structures::Route {
        id: "THB123_0".to_owned(),
        short_name: Some("1".to_owned()),
        long_name: Some("   ".to_owned()),
        ..Default::default()
    };
    assert_eq!(route_headsign_fallback(&route), "1");
}

#[test]
fn coverage_diagnostics_report_unused_stops_route_gaps_and_realtime_only_patterns() {
    let input = temporary_path("coverage.zip");
    write_coverage_fixture(&input, super::MAX_DIAGNOSTIC_EXAMPLES + 3);

    let (network, bindings, diagnostics) = super::build_network(&input).unwrap();

    assert_eq!(
        diagnostics.valid_unreferenced_stops,
        super::MAX_DIAGNOSTIC_EXAMPLES + 3
    );
    assert_eq!(
        diagnostics.unreferenced_stop_examples.len(),
        super::MAX_DIAGNOSTIC_EXAMPLES
    );
    assert_eq!(diagnostics.unreferenced_stop_examples[0], "UNUSED_000");
    assert_eq!(network.stops.len(), 2);
    assert_eq!(bindings.metro_patterns.len(), 1);

    let bus = &diagnostics.coverage_by_mode["bus"];
    assert_eq!(bus.supported_routes, 2);
    assert_eq!(bus.routes_without_patterns, 2);
    assert_eq!(bus.route_without_pattern_examples.len(), 2);

    let metro = &diagnostics.coverage_by_mode["metro"];
    assert_eq!(metro.patterns, 1);
    assert_eq!(metro.patterns_without_gtfs_service, 1);
    assert_eq!(metro.patterns_with_live_provider_binding, 1);
    assert_eq!(metro.patterns_without_usable_service, 0);
    assert_eq!(metro.estimated_or_mixed_timing_patterns, 1);
    assert_eq!(metro.patterns_without_gtfs_service_examples.len(), 1);
    assert_eq!(metro.estimated_or_mixed_timing_examples.len(), 1);

    std::fs::remove_file(input).unwrap();
}

#[test]
fn rejects_a_feed_with_no_usable_route_service_before_build_enrichment() {
    let input = temporary_path("no-service.zip");
    write_no_service_fixture(&input);

    let error = super::build_network(&input).unwrap_err();

    assert!(matches!(
        error,
        crate::data::DataError::NoUsableRouteService { .. }
    ));
    std::fs::remove_file(input).unwrap();
}

#[tokio::test]
async fn builds_network_timing_and_via_transfers_ignoring_gtfs_transfers() {
    let input = temporary_path("input.zip");
    let snapshot = temporary_path("fetch");
    let output = temporary_path("output");
    write_gtfs_fixture(&input);
    write_fetch_snapshot(&input, &snapshot);

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
    let built = build_from_fetch_snapshot(&snapshot, &output, &street)
        .await
        .unwrap();
    let network = &built.build.via_static.transit;
    let runtime = TransitRuntime::new(std::sync::Arc::new(network.clone()));
    assert_eq!(built.diagnostics.stops_loaded, 6);
    assert_eq!(built.diagnostics.trips_loaded, 4);
    assert_eq!(built.diagnostics.frequencies_loaded, 1);
    assert!(built.build.diagnostics.static_transfers.is_some());
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
        .filter(|pattern| runtime.route_of(pattern).mode == TransitMode::Bus)
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
    assert_eq!(runtime.route_of(bus).label, "1");
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
    let persisted = crate::data::load_static_artifact(&output.join(".via-static.json")).unwrap();
    assert_eq!(
        persisted
            .transit
            .patterns
            .iter()
            .find(|pattern| pattern.id == bus.id)
            .unwrap()
            .static_profile,
        bus.static_profile
    );
    assert_eq!(
        built.build.via_static.provider_bindings.bus_patterns.len(),
        2
    );
    assert_eq!(
        built
            .build
            .via_static
            .provider_bindings
            .metro_patterns
            .len(),
        1
    );
    assert!(
        network
            .stops
            .iter()
            .any(|stop| stop.id.as_str() == "TRTC-BL12")
    );
    let bus_binding = &built.build.via_static.provider_bindings.bus_patterns[&bus.id];
    assert_eq!(bus_binding.tdx_service_id, "THB123::direction=0");
    assert_eq!(bus_binding.realtime_scope, "Taipei");
    let metro_binding = built
        .build
        .via_static
        .provider_bindings
        .metro_patterns
        .values()
        .next()
        .unwrap();
    assert_eq!(metro_binding.line_id, "BL");
    assert_eq!(metro_binding.direction, 0);
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
    let calendar = runtime
        .calendar(&network.frequencies[0].calendar_id)
        .unwrap();
    assert!(calendar.runs_on(date!(2026 - 09 - 22)));
    assert!(!calendar.runs_on(date!(2026 - 09 - 23)));
    assert!(calendar.runs_on(date!(2026 - 09 - 24)));
    assert!(
        runtime
            .pattern(&crate::domain::ids::TransitPatternId::new(
                "pattern:metro:BL::direction=0::variant=0"
            ))
            .is_some()
    );
    assert!(
        runtime
            .pattern(&crate::domain::ids::TransitPatternId::new(
                "pattern:THB123::direction=0::variant=0"
            ))
            .is_some()
    );

    assert!(!output.join("provider-bindings.json").exists());
    assert!(!output.join("manifest.json").exists());
    assert_eq!(
        std::fs::read_dir(&output)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [".via-static.json"]
    );
    std::fs::remove_file(input).unwrap();
    std::fs::remove_dir_all(snapshot).unwrap();
    std::fs::remove_dir_all(output).unwrap();
}

#[tokio::test]
async fn rejects_incomplete_untracked_and_mismatched_fetch_snapshots() {
    let input = temporary_path("strict-input.zip");
    write_gtfs_fixture(&input);
    let street = FakeStreetRouter::builder().build();

    let untracked = temporary_path("untracked-fetch");
    write_fetch_snapshot(&input, &untracked);
    std::fs::write(untracked.join("bike-stations-taipei-page-0002.json"), "[]").unwrap();
    assert!(
        build_from_fetch_snapshot(&untracked, &temporary_path("untracked-output"), &street)
            .await
            .is_err()
    );
    std::fs::remove_dir_all(untracked).unwrap();

    let incomplete = temporary_path("incomplete-fetch");
    write_fetch_snapshot(&input, &incomplete);
    std::fs::remove_file(incomplete.join("bike-stations-taipei.json")).unwrap();
    assert!(
        build_from_fetch_snapshot(&incomplete, &temporary_path("incomplete-output"), &street)
            .await
            .is_err()
    );
    std::fs::remove_dir_all(incomplete).unwrap();

    let mismatched = temporary_path("mismatched-fetch");
    write_fetch_snapshot(&input, &mismatched);
    std::fs::write(mismatched.join("bike-stations-taipei.json"), "[ ]").unwrap();
    assert!(
        build_from_fetch_snapshot(&mismatched, &temporary_path("mismatched-output"), &street)
            .await
            .is_err()
    );
    std::fs::remove_dir_all(mismatched).unwrap();
    std::fs::remove_file(input).unwrap();
}

#[tokio::test]
async fn rejects_malformed_manifest_bike_json_and_gtfs_zip() {
    let input = temporary_path("malformed-input.zip");
    write_gtfs_fixture(&input);
    let street = FakeStreetRouter::builder().build();

    let malformed_manifest = temporary_path("malformed-manifest-fetch");
    write_fetch_snapshot(&input, &malformed_manifest);
    std::fs::write(malformed_manifest.join("manifest.json"), "{").unwrap();
    assert!(
        build_from_fetch_snapshot(
            &malformed_manifest,
            &temporary_path("malformed-manifest-output"),
            &street,
        )
        .await
        .is_err()
    );
    std::fs::remove_dir_all(malformed_manifest).unwrap();

    let malformed_bike = temporary_path("malformed-bike-fetch");
    write_fetch_snapshot(&input, &malformed_bike);
    std::fs::write(malformed_bike.join("bike-stations-taipei.json"), "{").unwrap();
    refresh_fetch_source(&malformed_bike, "bike-stations-taipei");
    assert!(
        build_from_fetch_snapshot(
            &malformed_bike,
            &temporary_path("malformed-bike-output"),
            &street,
        )
        .await
        .is_err()
    );
    std::fs::remove_dir_all(malformed_bike).unwrap();

    let malformed_gtfs = temporary_path("malformed-gtfs-fetch");
    write_fetch_snapshot(&input, &malformed_gtfs);
    std::fs::write(malformed_gtfs.join("gtfs.zip"), b"not a zip archive").unwrap();
    refresh_fetch_source(&malformed_gtfs, "gtfs");
    assert!(
        build_from_fetch_snapshot(
            &malformed_gtfs,
            &temporary_path("malformed-gtfs-output"),
            &street,
        )
        .await
        .is_err()
    );
    std::fs::remove_dir_all(malformed_gtfs).unwrap();
    std::fs::remove_file(input).unwrap();
}
