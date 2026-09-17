//! Offline TDX snapshot, normalization, and artifact tooling.
//!
//! Provider payloads stop at this module. The routing and domain modules only
//! receive the validated provider-neutral `TransitNetwork` artifact.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Date, Weekday, format_description::well_known::Rfc3339};

use crate::{
    config::{TdxClient, TdxClientError, TdxCredentials},
    domain::{
        ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
        transit::{
            BikeShareStation, MetroFrequency, MetroStationTiming, MetroTiming, ScheduledStopTime,
            ScheduledTrip, TransferEdge, TransitMode, TransitNetwork, TransitService, TransitStop,
            default_service_day_boundary,
        },
    },
    providers::tdx::{
        bike_share_station_id, boarding_point_id, transit_service_id,
        transit_service_id_for_direction,
    },
    routing::transfers::TransferShortcutIndex,
};

pub const ARTIFACT_SCHEMA_VERSION: u32 = 2;
const TDX_REQUEST_SPACING: Duration = Duration::from_secs(13);
const TDX_RETRY_BACKOFFS: [Duration; 4] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];
const SECONDS_PER_SERVICE_DAY: u32 = 86_400;
const ALL_WEEKDAYS: [Weekday; 7] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
    Weekday::Sunday,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataScope {
    TaipeiNewTaipei,
}

impl DataScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaipeiNewTaipei => "taipei-new-taipei",
        }
    }

    fn cities(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::TaipeiNewTaipei => &[("taipei", "Taipei"), ("new-taipei", "NewTaipei")],
        }
    }
}

impl FromStr for DataScope {
    type Err = DataError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "taipei-new-taipei" => Ok(Self::TaipeiNewTaipei),
            _ => Err(DataError::InvalidScope(value.into())),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactCounts {
    pub stops: usize,
    pub services: usize,
    pub scheduled_trips: usize,
    pub bike_stations: usize,
    pub transfer_edges: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRevisions {
    pub static_network_revision: String,
    pub schedule_revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifest {
    pub schema_version: u32,
    pub scope: String,
    pub built_at: String,
    pub raw_snapshot_date: Option<String>,
    pub sources: BTreeMap<String, String>,
    pub counts: ArtifactCounts,
    pub revisions: ArtifactRevisions,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawSnapshotManifest {
    pub schema_version: u32,
    pub scope: String,
    pub date: String,
    pub sources: BTreeMap<String, RawSourceMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawSourceMetadata {
    pub endpoint: String,
    pub path: String,
    pub sha256: String,
    pub retrieved_at: String,
}

#[derive(Debug)]
pub struct RawSnapshotFetchReport {
    pub manifest: RawSnapshotManifest,
    pub reused_count: usize,
    pub downloaded_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchProgress {
    Reuse {
        source_key: String,
        relative_path: String,
    },
    Fetch {
        source_key: String,
    },
    Wait {
        duration: Duration,
    },
    Retry {
        source_key: String,
        duration: Duration,
    },
}

#[derive(Clone, Debug)]
pub struct BuildOutput {
    pub network: TransitNetwork,
    pub manifest: ArtifactManifest,
    pub transfer_index: TransferShortcutIndex,
    pub diagnostics: BuildDiagnostics,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildDiagnostics {
    pub skipped_services: Vec<SkippedService>,
    pub malformed_bus_records: usize,
    pub unresolved_bus_stop_references: usize,
    pub duplicate_bus_stop_artifacts_removed: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedService {
    pub service_id: String,
    pub reason: String,
    pub stops: usize,
}

#[derive(Debug, Error)]
pub enum DataError {
    #[error("unsupported data scope {0:?}; supported scope is taipei-new-taipei")]
    InvalidScope(String),
    #[error("date must use YYYY-MM-DD: {0}")]
    InvalidDate(String),
    #[error("invalid coordinate: {0}")]
    InvalidCoordinate(String),
    #[error("data path {0} could not be read: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("JSON in {0} is invalid: {1}")]
    Json(PathBuf, #[source] serde_json::Error),
    #[error("TDX fetch failed for {source_name}: {error}")]
    Fetch {
        source_name: String,
        error: TdxClientError,
    },
    #[error("raw snapshot {0} contains no JSON source files")]
    EmptySnapshot(PathBuf),
    #[error("raw snapshot has no usable transit stops")]
    EmptyNetwork,
    #[error("raw source {path} has malformed {kind}: {detail}")]
    MalformedSource {
        path: PathBuf,
        kind: &'static str,
        detail: String,
    },
    #[error("raw source {path} contains conflicting identity {id}: {detail}")]
    ConflictingIdentity {
        path: PathBuf,
        id: String,
        detail: String,
    },
    #[error("normalized network failed validation: {0}")]
    InvalidArtifact(#[from] crate::domain::transit::TransitNetworkValidationError),
    #[error("artifact JSON is invalid: {0}")]
    InvalidArtifactJson(String),
}

#[derive(Clone, Debug)]
struct StaticSource {
    key: String,
    path: String,
    query: Vec<(String, String)>,
    endpoint: String,
    output_path: PathBuf,
    realtime_scope: Option<String>,
}

#[derive(Debug)]
struct RawDocument {
    relative_path: PathBuf,
    bytes: Vec<u8>,
    value: Value,
    realtime_scope: Option<String>,
}

#[derive(Clone, Debug)]
struct MetroPattern {
    line_id: String,
    direction: Option<u8>,
    label: String,
    headsign: String,
    stations: Vec<(u32, BoardingPointId)>,
}

type MetroTimingKey = (String, Option<u8>, String);
type MetroFrequencyRange = (u32, u32, u32);
type MetroTravelKey = (String, Option<u8>, String, String);
type MetroRouteKey = (String, String);

#[derive(Default)]
struct Normalizer {
    stops: BTreeMap<String, TransitStop>,
    services: BTreeMap<String, TransitService>,
    trips: BTreeMap<String, ScheduledTrip>,
    bike_stations: BTreeMap<String, BikeShareStation>,
    transfer_edges: BTreeSet<(String, String, u32)>,
    station_aliases: BTreeMap<String, BoardingPointId>,
    metro_patterns: BTreeMap<(String, Option<u8>), MetroPattern>,
    metro_departures: BTreeMap<MetroTimingKey, BTreeSet<u32>>,
    metro_frequencies: BTreeMap<MetroTimingKey, BTreeSet<MetroFrequencyRange>>,
    metro_travel_times: BTreeMap<MetroTravelKey, u32>,
    metro_route_paths: BTreeMap<MetroRouteKey, Vec<BoardingPointId>>,
    metro_headsigns: BTreeMap<(String, Option<u8>, String), String>,
    diagnostics: BuildDiagnostics,
    skipped_services: BTreeSet<String>,
    bus_service_records: usize,
    bus_affected_service_ids: BTreeSet<String>,
    bus_source_path: Option<PathBuf>,
}

impl Normalizer {
    fn normalize_document(&mut self, document: &RawDocument) -> Result<(), DataError> {
        let path_text = document
            .relative_path
            .to_string_lossy()
            .to_ascii_lowercase();
        let realtime_scope = document.realtime_scope.as_deref();
        let records = records(&document.value);
        for (index, record) in records.iter().enumerate() {
            if path_text.contains("bike") {
                self.normalize_bike(record, &document.relative_path)?;
            }
            if path_text.contains("bus") {
                self.normalize_bus(record, &document.relative_path, index, realtime_scope)?;
            }
            if is_rail_path(&path_text) {
                self.normalize_rail(record, &document.relative_path, index, &path_text)?;
            }
            if is_metro_station_timetable_path(&path_text)
                || path_text.contains("s2straveltime")
                || (path_text.contains("s2s") && path_text.contains("travel"))
                || path_text.contains("frequency")
            {
                self.normalize_metro_timing(record, &path_text)?;
            }
            if path_text.contains("transfer") {
                if path_text.contains("line-transfer") {
                    self.normalize_metro_line_transfer(record, &document.relative_path)?;
                } else {
                    self.normalize_transfer(record, &document.relative_path)?;
                }
            }
        }
        Ok(())
    }

    fn normalize_bike(&mut self, record: &Value, path: &Path) -> Result<(), DataError> {
        let Some(station_uid) = string(record, "StationUID") else {
            return Ok(());
        };
        let coordinate =
            coordinate(record).ok_or_else(|| malformed(path, "bike station", station_uid))?;
        let station = BikeShareStation::new(
            bike_share_station_id(station_uid),
            display_name(record, &["StationName", "StationNameEn"])
                .unwrap_or_else(|| station_uid.into()),
            coordinate,
        );
        let key = station.id.as_str().to_owned();
        if let Some(existing) = self.bike_stations.get(&key) {
            if existing != &station {
                return Err(DataError::ConflictingIdentity {
                    path: path.into(),
                    id: key,
                    detail: "BikeShareStation records disagree".into(),
                });
            }
        } else {
            self.bike_stations.insert(key, station);
        }
        Ok(())
    }

    fn normalize_bus(
        &mut self,
        record: &Value,
        path: &Path,
        index: usize,
        realtime_scope: Option<&str>,
    ) -> Result<(), DataError> {
        if let Some(stops_value) = record.get("Stops") {
            self.bus_service_records += 1;
            self.bus_source_path
                .get_or_insert_with(|| path.to_path_buf());
            let Some(stops) = stops_value.as_array() else {
                self.diagnostics.malformed_bus_records += 1;
                return Ok(());
            };
            let sub_route = string(record, "SubRouteUID")
                .or_else(|| string(record, "ServiceUID"))
                .filter(|value| !value.trim().is_empty());
            let Some(sub_route) = sub_route else {
                self.diagnostics.malformed_bus_records += 1;
                return Ok(());
            };
            let direction = number(record, "Direction").and_then(|value| u8::try_from(value).ok());
            let malformed_direction = record.get("Direction").is_none() || direction.is_none();
            if malformed_direction {
                self.diagnostics.malformed_bus_records += 1;
            }
            let service_id = direction
                .map(|direction| transit_service_id_for_direction(sub_route, direction))
                .unwrap_or_else(|| transit_service_id(sub_route));
            if malformed_direction {
                self.bus_affected_service_ids
                    .insert(service_id.as_str().into());
            }
            let mut seen_sequences = BTreeSet::new();
            let mut ordered = Vec::with_capacity(stops.len());
            for (fallback, stop) in stops.iter().enumerate() {
                let sequence = number(stop, "StopSequence");
                if sequence.is_none_or(|sequence| !seen_sequences.insert(sequence)) {
                    self.diagnostics.malformed_bus_records += 1;
                    self.bus_affected_service_ids
                        .insert(service_id.as_str().into());
                }
                ordered.push((sequence.unwrap_or(fallback as u64), stop));
            }
            ordered.sort_by_key(|(sequence, _)| *sequence);
            let mut pattern = Vec::with_capacity(ordered.len());
            // A repeated UID at a different sequence is a legitimate loop stop.
            // Only an exact sequence/UID repeat is an empty duplicate artifact.
            let mut seen_sequence_stops = BTreeSet::new();
            for (sequence, stop) in ordered {
                let stop_id = match self.normalize_stop(stop, path, "bus") {
                    Ok(stop_id) => stop_id,
                    Err(DataError::MalformedSource { .. }) => {
                        self.diagnostics.unresolved_bus_stop_references += 1;
                        self.bus_affected_service_ids
                            .insert(service_id.as_str().into());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if !seen_sequence_stops.insert((sequence, stop_id.as_str().to_owned())) {
                    self.diagnostics.duplicate_bus_stop_artifacts_removed += 1;
                    continue;
                }
                pattern.push(stop_id);
            }
            if distinct_stop_count(&pattern) < 2 {
                self.bus_affected_service_ids
                    .insert(service_id.as_str().into());
            }
            let mut service = TransitService::new(
                service_id.clone(),
                TransitMode::Bus,
                display_name(record, &["SubRouteName", "RouteName"])
                    .unwrap_or_else(|| sub_route.into()),
                display_name(record, &["DestinationStopNameZh", "DestinationStopName"])
                    .unwrap_or_default(),
                pattern,
            )
            .with_timing_source("bus_n1");
            if let Some(scope) = realtime_scope {
                service = service.with_realtime_scope(scope);
            }
            let inserted = self.insert_service(service, path)?;
            if !inserted {
                return Ok(());
            }
            self.normalize_embedded_trip(record, path, index, TransitMode::Bus, realtime_scope)?;
            return Ok(());
        }
        if is_bus_stop_catalog_path(path) {
            if record.get("StopUID").is_none() {
                self.diagnostics.malformed_bus_records += 1;
                return Ok(());
            }
            match self.normalize_stop(record, path, "bus") {
                Ok(_) => {}
                Err(DataError::MalformedSource { .. }) => {
                    self.diagnostics.malformed_bus_records += 1;
                }
                Err(error) => return Err(error),
            }
            return Ok(());
        }
        if string(record, "StopUID").is_some() {
            match self.normalize_stop(record, path, "bus") {
                Ok(_) => {}
                Err(DataError::MalformedSource { .. }) => {
                    self.diagnostics.malformed_bus_records += 1;
                }
                Err(error) => return Err(error),
            }
        }
        if record.get("StopTimes").is_some()
            || record.get("stop_times").is_some()
            || record.get("Timetables").and_then(Value::as_array).is_some()
        {
            self.normalize_embedded_trip(record, path, index, TransitMode::Bus, realtime_scope)?;
        }
        Ok(())
    }

    fn normalize_rail(
        &mut self,
        record: &Value,
        path: &Path,
        index: usize,
        path_text: &str,
    ) -> Result<(), DataError> {
        let mode = if path_text.contains("thsr") {
            TransitMode::HighSpeedRail
        } else if path_text.contains("metro") {
            TransitMode::Metro
        } else {
            TransitMode::Rail
        };
        if let Some(general_timetable) = record.get("GeneralTimetable") {
            self.normalize_embedded_trip(general_timetable, path, index, mode, None)?;
            return Ok(());
        }
        if let Some(station_uid) = string(record, "StationUID") {
            let id = self.normalize_rail_station(record, station_uid, path, mode)?;
            if let Some(line_id) = string(record, "LineID").or_else(|| string(record, "RouteUID")) {
                let sequence = number(record, "StationSequence")
                    .or_else(|| number(record, "Sequence"))
                    .unwrap_or(index as u64) as u32;
                let direction = number(record, "Direction")
                    .or_else(|| string(record, "Direction").and_then(|value| value.parse().ok()))
                    .and_then(|value| u8::try_from(value).ok());
                let key = (line_id.into(), direction);
                let entry = self
                    .metro_patterns
                    .entry(key)
                    .or_insert_with(|| MetroPattern {
                        line_id: line_id.into(),
                        direction,
                        label: display_name(record, &["LineName", "LineNameZh", "LineNameEn"])
                            .unwrap_or_else(|| line_id.into()),
                        headsign: String::new(),
                        stations: Vec::new(),
                    });
                entry.stations.push((sequence, id));
            }
        }
        if mode == TransitMode::Metro {
            self.normalize_metro_line_stations(record);
        }
        let has_embedded_stations = record
            .get("Stations")
            .and_then(Value::as_array)
            .is_some_and(|stations| {
                mode != TransitMode::Metro
                    || stations.iter().any(|station| {
                        station.get("Time").is_some()
                            || station.get("ArrivalTime").is_some()
                            || station.get("DepartureTime").is_some()
                    })
            });
        if has_embedded_stations
            || record.get("StopTimes").is_some()
            || record.get("stop_times").is_some()
            || record.get("TrainInfo").is_some()
            || record.get("GeneralTrainInfo").is_some()
            || record.get("DailyTrainInfo").is_some()
            || (record.get("Timetables").is_some() && !is_metro_station_timetable_path(path_text))
        {
            self.normalize_embedded_trip(record, path, index, mode, None)?;
        }
        Ok(())
    }

    fn normalize_metro_line_stations(&mut self, record: &Value) {
        let Some(line_id) = metro_line_id(record) else {
            return;
        };
        let Some(stations) = record.get("Stations").and_then(Value::as_array) else {
            return;
        };
        let direction = metro_direction(record);
        let label = display_name(record, &["LineName", "LineNameZh", "LineNameEn"])
            .unwrap_or_else(|| line_id.clone());
        for (fallback, station) in stations.iter().enumerate() {
            let Some(station_value) =
                string(station, "StationUID").or_else(|| string(station, "StationID"))
            else {
                continue;
            };
            let station_id = if string(station, "StationUID").is_some() {
                boarding_point_id(station_value)
            } else {
                metro_station_id(self, station, &["StationUID", "StationID"])
                    .map(BoardingPointId::new)
                    .unwrap_or_else(|| boarding_point_id(station_value))
            };
            if let Some(local_id) = string(station, "StationID") {
                self.station_aliases
                    .entry(local_id.into())
                    .or_insert_with(|| station_id.clone());
            }
            let sequence = number(station, "Sequence")
                .or_else(|| number(station, "StationSequence"))
                .unwrap_or(fallback as u64) as u32;
            let entry = self
                .metro_patterns
                .entry((line_id.clone(), direction))
                .or_insert_with(|| MetroPattern {
                    line_id: line_id.clone(),
                    direction,
                    label: label.clone(),
                    headsign: String::new(),
                    stations: Vec::new(),
                });
            entry.stations.push((sequence, station_id));
        }
    }

    fn normalize_metro_timing(&mut self, record: &Value, path_text: &str) -> Result<(), DataError> {
        let Some(line_id) = metro_line_id(record) else {
            return Ok(());
        };
        let direction = metro_direction(record);
        if path_text.contains("s2straveltime")
            || (path_text.contains("s2s") && path_text.contains("travel"))
        {
            if let Some(values) = record
                .get("TravelTimes")
                .or_else(|| record.get("S2STravelTimes"))
                .and_then(Value::as_array)
            {
                let route_id = metro_route_id(record).unwrap_or_else(|| line_id.clone());
                self.collect_metro_route_path(&line_id, &route_id, values);
            }
            if let (Some(from), Some(to), Some(seconds)) = (
                metro_station_id(
                    self,
                    record,
                    &["FromStationUID", "FromStationID", "StationUID", "StationID"],
                ),
                metro_station_id(
                    self,
                    record,
                    &[
                        "ToStationUID",
                        "ToStationID",
                        "NextStationUID",
                        "NextStationID",
                    ],
                ),
                metro_duration_seconds(record),
            ) {
                self.metro_travel_times
                    .insert((line_id.clone(), direction, from, to), seconds);
            }
            for key in ["TravelTimes", "S2STravelTimes"] {
                if let Some(values) = record.get(key).and_then(Value::as_array) {
                    for value in values {
                        let Some(object) = value.as_object() else {
                            continue;
                        };
                        let mut expanded = Value::Object(object.clone());
                        let Some(expanded_object) = expanded.as_object_mut() else {
                            continue;
                        };
                        expanded_object
                            .entry("LineID")
                            .or_insert_with(|| Value::String(line_id.clone()));
                        if let Some(direction) = direction {
                            expanded_object
                                .entry("Direction")
                                .or_insert_with(|| Value::from(direction));
                        }
                        self.normalize_metro_timing(&expanded, path_text)?;
                    }
                }
            }
            return Ok(());
        }

        let station_id = metro_station_id(self, record, &["StationUID", "StationID"])
            .unwrap_or_else(|| "*".into());
        let key = (line_id, direction, station_id);
        if is_metro_station_timetable_path(path_text) {
            if let Some(headsign) = display_name(
                record,
                &[
                    "DestinationStationName",
                    "DestinationStopNameZh",
                    "DestinationStopName",
                ],
            ) {
                let destination = metro_station_id(
                    self,
                    record,
                    &[
                        "DestinationStationUID",
                        "DestinationStaionID",
                        "DestinationStationID",
                    ],
                )
                .unwrap_or_else(|| "*".into());
                self.metro_headsigns
                    .entry((key.0.clone(), key.1, destination))
                    .or_insert(headsign);
            }
            self.collect_metro_departures(record, &key);
        }
        if path_text.contains("frequency") || record.get("Frequency").is_some() {
            self.collect_metro_frequencies(record, &key);
        }
        Ok(())
    }

    fn collect_metro_route_path(&mut self, line_id: &str, route_id: &str, values: &[Value]) {
        let mut edges = values
            .iter()
            .enumerate()
            .filter_map(|(fallback, value)| {
                Some((
                    number(value, "Sequence").unwrap_or(fallback as u64) as u32,
                    BoardingPointId::new(metro_station_id(
                        self,
                        value,
                        &["FromStationUID", "FromStationID", "StationUID", "StationID"],
                    )?),
                    BoardingPointId::new(metro_station_id(
                        self,
                        value,
                        &[
                            "ToStationUID",
                            "ToStationID",
                            "NextStationUID",
                            "NextStationID",
                        ],
                    )?),
                ))
            })
            .collect::<Vec<_>>();
        edges.sort_by_key(|(sequence, _, _)| *sequence);
        let Some((_, first_from, first_to)) = edges.first() else {
            return;
        };
        let mut path = vec![first_from.clone(), first_to.clone()];
        for (_, from, to) in edges.into_iter().skip(1) {
            if path.last() != Some(&from) {
                return;
            }
            path.push(to);
        }
        let key = (line_id.into(), route_id.into());
        if self
            .metro_route_paths
            .get(&key)
            .is_none_or(|existing| existing.len() < path.len())
        {
            self.metro_route_paths.insert(key, path);
        }
    }

    fn collect_metro_departures(&mut self, record: &Value, key: &MetroTimingKey) {
        for field in ["Time", "DepartureTime", "Departure", "TrainTime"] {
            if let Some(seconds) = record.get(field).and_then(time_seconds_value) {
                self.metro_departures
                    .entry(key.clone())
                    .or_default()
                    .insert(seconds);
            }
        }
        for field in ["DepartureTimes", "Departures", "Timetables", "Timetable"] {
            let Some(values) = record.get(field).and_then(Value::as_array) else {
                continue;
            };
            for value in values {
                if let Some(seconds) = time_seconds_value(value) {
                    self.metro_departures
                        .entry(key.clone())
                        .or_default()
                        .insert(seconds);
                } else if let Some(object) = value.as_object() {
                    self.collect_metro_departures(&Value::Object(object.clone()), key);
                }
            }
        }
    }

    fn collect_metro_frequencies(&mut self, record: &Value, key: &MetroTimingKey) {
        if let Some(frequency) = metro_frequency(record) {
            self.metro_frequencies
                .entry(key.clone())
                .or_default()
                .insert(frequency);
        }
        for field in ["Frequency", "Frequencies", "Headways"] {
            if let Some(values) = record.get(field).and_then(Value::as_array) {
                for value in values {
                    if let Some(object) = value.as_object() {
                        let mut expanded = Value::Object(object.clone());
                        if let Some(operation_time) =
                            record.get("OperationTime").and_then(Value::as_object)
                        {
                            let Some(expanded_object) = expanded.as_object_mut() else {
                                continue;
                            };
                            for (field, operation_field) in
                                [("StartTime", "StartTime"), ("EndTime", "EndTime")]
                            {
                                if !expanded_object.contains_key(field) {
                                    if let Some(value) = operation_time.get(operation_field) {
                                        expanded_object.insert(field.into(), value.clone());
                                    }
                                }
                            }
                        }
                        self.collect_metro_frequencies(&expanded, key);
                    }
                }
            } else if let Some(object) = record.get(field).and_then(Value::as_object) {
                self.collect_metro_frequencies(&Value::Object(object.clone()), key);
            }
        }
    }

    fn canonical_metro_stop_id(&self, station_id: &str) -> String {
        self.station_aliases
            .get(station_id)
            .map(|id| id.as_str().to_owned())
            .unwrap_or_else(|| station_id.to_owned())
    }

    fn metro_travel_seconds(
        &self,
        line_id: &str,
        direction: Option<u8>,
        pattern: &[BoardingPointId],
    ) -> Vec<u32> {
        pattern
            .windows(2)
            .filter_map(|stations| {
                let matches_stations = |from: &str, to: &str| {
                    self.canonical_metro_stop_id(from) == stations[0].as_str()
                        && self.canonical_metro_stop_id(to) == stations[1].as_str()
                };
                let matches_reversed_stations = |from: &str, to: &str| {
                    self.canonical_metro_stop_id(from) == stations[1].as_str()
                        && self.canonical_metro_stop_id(to) == stations[0].as_str()
                };
                self.metro_travel_times
                    .iter()
                    .find(|((line, record_direction, from, to), _)| {
                        line == line_id
                            && (record_direction == &direction || record_direction.is_none())
                            && matches_stations(from, to)
                    })
                    .or_else(|| {
                        self.metro_travel_times.iter().find(
                            |((line, record_direction, from, to), _)| {
                                line == line_id
                                    && (record_direction == &direction
                                        || record_direction.is_none())
                                    && matches_reversed_stations(from, to)
                            },
                        )
                    })
                    .or_else(|| {
                        self.metro_travel_times
                            .iter()
                            .find(|((line, _, from, to), _)| {
                                line == line_id && matches_stations(from, to)
                            })
                    })
                    .or_else(|| {
                        self.metro_travel_times
                            .iter()
                            .find(|((line, _, from, to), _)| {
                                line == line_id && matches_reversed_stations(from, to)
                            })
                    })
                    .map(|(_, seconds)| *seconds)
            })
            .collect()
    }

    fn metro_station_timings(
        &self,
        line_id: &str,
        direction: Option<u8>,
        pattern: &[BoardingPointId],
    ) -> Vec<MetroStationTiming> {
        pattern
            .iter()
            .filter_map(|stop_id| {
                let mut departures = BTreeSet::new();
                let mut frequencies = BTreeSet::new();
                for ((line, record_direction, station), values) in &self.metro_departures {
                    if line == line_id
                        && (record_direction == &direction
                            || (direction.is_none() && record_direction.is_none()))
                        && (self.canonical_metro_stop_id(station) == stop_id.as_str()
                            || station == "*")
                    {
                        departures.extend(values);
                    }
                }
                for ((line, record_direction, station), values) in &self.metro_frequencies {
                    if line == line_id
                        && (direction.is_none()
                            || record_direction == &direction
                            || record_direction.is_none())
                        && (self.canonical_metro_stop_id(station) == stop_id.as_str()
                            || station == "*")
                    {
                        frequencies.extend(values);
                    }
                }
                (!departures.is_empty() || !frequencies.is_empty()).then(|| MetroStationTiming {
                    stop_id: stop_id.clone(),
                    departures_seconds: departures.into_iter().collect(),
                    frequencies: frequencies
                        .into_iter()
                        .map(
                            |(start_seconds, end_seconds, headway_seconds)| MetroFrequency {
                                start_seconds,
                                end_seconds,
                                headway_seconds,
                            },
                        )
                        .collect(),
                })
            })
            .collect()
    }

    fn normalize_rail_station(
        &mut self,
        record: &Value,
        station_uid: &str,
        path: &Path,
        mode: TransitMode,
    ) -> Result<BoardingPointId, DataError> {
        let id = boarding_point_id(station_uid);
        if let Some(coordinate) = coordinate(record) {
            let station = TransitStop::new(
                id.clone(),
                display_name(record, &["StationName", "StationNameZh", "StationNameEn"])
                    .unwrap_or_else(|| station_uid.into()),
                coordinate,
            );
            self.insert_stop(station, path)?;
        } else if !self.stops.contains_key(id.as_str()) {
            return Err(malformed(path, "rail station", station_uid));
        }
        if let Some(station_id) = string(record, "StationID") {
            self.station_aliases
                .insert(format!("{mode:?}:{station_id}"), id.clone());
            self.station_aliases.insert(station_id.into(), id.clone());
        }
        Ok(id)
    }

    fn normalize_embedded_trip(
        &mut self,
        record: &Value,
        path: &Path,
        index: usize,
        mode: TransitMode,
        realtime_scope: Option<&str>,
    ) -> Result<(), DataError> {
        let stop_times = record
            .get("StopTimes")
            .or_else(|| record.get("stop_times"))
            .or_else(|| record.get("Stations"))
            .and_then(Value::as_array);
        let Some(stop_times) = stop_times else {
            if let Some(timetables) = record.get("Timetables").and_then(Value::as_array) {
                for (timetable_index, timetable) in timetables.iter().enumerate() {
                    let Some(timetable_object) = timetable.as_object() else {
                        return Err(malformed(path, "timetable", timetable_index.to_string()));
                    };
                    let mut expanded = record.clone();
                    let Some(expanded_object) = expanded.as_object_mut() else {
                        return Err(malformed(path, "timetable", timetable_index.to_string()));
                    };
                    for (key, value) in timetable_object {
                        expanded_object.insert(key.clone(), value.clone());
                    }
                    self.normalize_embedded_trip(
                        &expanded,
                        path,
                        index
                            .saturating_mul(100_000)
                            .saturating_add(timetable_index),
                        mode,
                        realtime_scope,
                    )?;
                }
            }
            return Ok(());
        };
        let operator = match mode {
            TransitMode::HighSpeedRail => "thsr",
            TransitMode::Rail => "tra",
            TransitMode::Metro => "metro",
            TransitMode::Bus => "bus",
        };
        let train_info = record
            .get("TrainInfo")
            .or_else(|| record.get("GeneralTrainInfo"))
            .or_else(|| record.get("DailyTrainInfo"))
            .unwrap_or(record);
        let train_no = string(train_info, "TrainNo")
            .or_else(|| string(record, "TripID"))
            .or_else(|| string(record, "TripId"))
            .or_else(|| string(record, "ServiceUID"))
            .or_else(|| string(record, "SubRouteUID"))
            .unwrap_or("trip");
        let overnight = train_info
            .get("Overnight")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || string(train_info, "OverNightStationID").is_some();
        let service_id = if mode == TransitMode::Bus {
            let sub_route = string(record, "SubRouteUID").unwrap_or(train_no);
            number(record, "Direction")
                .and_then(|direction| u8::try_from(direction).ok())
                .map(|direction| transit_service_id_for_direction(sub_route, direction))
                .or_else(|| {
                    self.services
                        .values()
                        .find(|service| {
                            service.id.as_str() == sub_route
                                || service
                                    .id
                                    .as_str()
                                    .starts_with(&format!("{sub_route}::direction="))
                        })
                        .map(|service| service.id.clone())
                })
                .unwrap_or_else(|| transit_service_id(sub_route))
        } else {
            TransitServiceId::new(format!("{operator}:{train_no}"))
        };
        if stop_times.len() < 2 {
            return Ok(());
        }
        let service_label = if mode == TransitMode::Bus {
            let sub_route = string(record, "SubRouteUID").unwrap_or(train_no);
            display_name(record, &["SubRouteName", "RouteName"])
                .or_else(|| {
                    self.services
                        .get(service_id.as_str())
                        .map(|service| service.label.clone())
                })
                .unwrap_or_else(|| sub_route.into())
        } else {
            train_no.into()
        };
        let headsign = display_name(record, &["DestinationStopNameZh", "DestinationStopName"])
            .or_else(|| {
                self.services
                    .get(service_id.as_str())
                    .map(|service| service.headsign.clone())
            })
            .unwrap_or_default();
        let mut parsed = Vec::with_capacity(stop_times.len());
        let mut previous_departure = 0;
        for (fallback, stop_time) in stop_times.iter().enumerate() {
            let stop_id = match self.resolve_timetable_stop(stop_time, path, mode, fallback) {
                Ok(stop_id) => stop_id,
                Err(DataError::MalformedSource { .. }) if mode == TransitMode::Bus => {
                    self.diagnostics.unresolved_bus_stop_references += 1;
                    self.bus_affected_service_ids
                        .insert(service_id.as_str().into());
                    continue;
                }
                Err(error) => return Err(error),
            };
            let raw_departure = time_seconds(
                string(stop_time, "DepartureTime")
                    .or_else(|| string(stop_time, "departure_time"))
                    .or_else(|| string(stop_time, "Time")),
            )
            .or_else(|| {
                time_seconds(
                    string(stop_time, "ArrivalTime").or_else(|| string(stop_time, "arrival_time")),
                )
            })
            .ok_or_else(|| malformed(path, "scheduled departure time", train_no))?;
            let raw_arrival = time_seconds(
                string(stop_time, "ArrivalTime")
                    .or_else(|| string(stop_time, "arrival_time"))
                    .or_else(|| string(stop_time, "Time")),
            )
            .unwrap_or(raw_departure);
            let arrival = if overnight {
                lift_time_after(raw_arrival, previous_departure)
                    .ok_or_else(|| malformed(path, "scheduled arrival time", train_no))?
            } else {
                raw_arrival
            };
            let departure = if overnight {
                lift_time_after(raw_departure, arrival)
                    .ok_or_else(|| malformed(path, "scheduled departure time", train_no))?
            } else {
                raw_departure
            };
            previous_departure = departure;
            parsed.push((stop_id, ScheduledStopTime::new(arrival, departure)));
        }
        let pattern = parsed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        if distinct_stop_count(&pattern) < 2 {
            return Ok(());
        }
        let mut service = TransitService::new(
            service_id.clone(),
            mode,
            service_label,
            if mode == TransitMode::Bus {
                headsign
            } else {
                display_name(train_info, &["EndingStationName", "DestinationStopNameZh"])
                    .unwrap_or_default()
            },
            pattern,
        )
        .with_timing_source(match mode {
            TransitMode::Metro => "metro_schedule",
            TransitMode::Rail => "tra_schedule",
            TransitMode::HighSpeedRail => "thsr_schedule",
            TransitMode::Bus => "bus_n1",
        });
        if mode == TransitMode::Bus {
            if let Some(scope) = realtime_scope {
                service = service.with_realtime_scope(scope);
            }
        }
        let inserted = self.insert_service(service, path)?;
        if !inserted {
            return Ok(());
        }
        let trip_id = string(train_info, "TrainDate")
            .map(|date| format!("{operator}:{train_no}:{date}"))
            .unwrap_or_else(|| format!("{operator}:{train_no}:{index}"));
        let (active_weekdays, added_days, removed_days) = service_calendar(record);
        let trip = ScheduledTrip::weekdays(
            ScheduledTripId::new(trip_id),
            service_id,
            active_weekdays,
            parsed.into_iter().map(|(_, time)| time).collect(),
        )
        .with_service_day_exceptions(added_days, removed_days);
        self.insert_trip(trip, path)
    }

    fn resolve_timetable_stop(
        &mut self,
        record: &Value,
        path: &Path,
        mode: TransitMode,
        index: usize,
    ) -> Result<BoardingPointId, DataError> {
        if let Some(station_uid) =
            string(record, "StationUID").or_else(|| string(record, "StopUID"))
        {
            if station_uid.trim().is_empty() {
                return Err(malformed(path, "timetable stop", "empty stop ID"));
            }
            let id = boarding_point_id(station_uid);
            if let Some(coordinate) = coordinate(record) {
                let stop = TransitStop::new(
                    id.clone(),
                    display_name(record, &["StationName", "StopName"])
                        .unwrap_or_else(|| station_uid.into()),
                    coordinate,
                );
                self.insert_stop(stop, path)?;
            } else if !self.stops.contains_key(id.as_str()) {
                return Err(malformed(path, "timetable stop", station_uid));
            }
            return Ok(id);
        }
        let local = string(record, "StationID").or_else(|| string(record, "StopID"));
        if let Some(local) = local {
            if local.trim().is_empty() {
                return Err(malformed(path, "timetable stop", "empty stop ID"));
            }
            if let Some(id) = self
                .station_aliases
                .get(&format!("{mode:?}:{local}"))
                .or_else(|| self.station_aliases.get(local))
            {
                return Ok(id.clone());
            }
            let id = boarding_point_id(format!("{mode:?}:{local}"));
            if let Some(coordinate) = coordinate(record) {
                self.insert_stop(
                    TransitStop::new(
                        id.clone(),
                        display_name(record, &["StationName", "StopName"])
                            .unwrap_or_else(|| local.into()),
                        coordinate,
                    ),
                    path,
                )?;
            } else {
                return Err(malformed(path, "timetable stop", local));
            }
            self.station_aliases.insert(local.into(), id.clone());
            return Ok(id);
        }
        Err(malformed(path, "timetable stop", index.to_string()))
    }

    fn normalize_stop(
        &mut self,
        record: &Value,
        path: &Path,
        kind: &'static str,
    ) -> Result<BoardingPointId, DataError> {
        let stop_uid =
            string(record, "StopUID").ok_or_else(|| malformed(path, kind, "missing StopUID"))?;
        if stop_uid.trim().is_empty() {
            return Err(malformed(path, kind, "empty StopUID"));
        }
        let id = boarding_point_id(stop_uid);
        let Some(coordinate) =
            coordinate(record).or_else(|| self.stops.get(id.as_str()).map(|stop| stop.coordinate))
        else {
            return Err(malformed(path, kind, stop_uid));
        };
        let stop = TransitStop::new(
            id.clone(),
            display_name(record, &["StopName", "StopNameZh", "StopNameEn"])
                .or_else(|| self.stops.get(id.as_str()).map(|stop| stop.name.clone()))
                .unwrap_or_else(|| stop_uid.into()),
            coordinate,
        );
        self.insert_stop(stop, path)?;
        Ok(id)
    }

    fn insert_stop(&mut self, stop: TransitStop, path: &Path) -> Result<(), DataError> {
        let key = stop.id.as_str().to_owned();
        if let Some(existing) = self.stops.get(&key) {
            if existing != &stop {
                return Err(DataError::ConflictingIdentity {
                    path: path.into(),
                    id: key,
                    detail: "TransitStop records disagree".into(),
                });
            }
        } else {
            self.stops.insert(key, stop);
        }
        Ok(())
    }

    fn insert_service(&mut self, service: TransitService, path: &Path) -> Result<bool, DataError> {
        let key = service.id.as_str().to_owned();
        let usable_stops = distinct_stop_count(&service.pattern);
        if usable_stops < 2 {
            self.skip_service(&service.id, "pattern_too_short", usable_stops);
            return Ok(false);
        }
        if self.skipped_services.contains(&key) {
            return Ok(false);
        }
        if let Some(existing) = self.services.get(&key) {
            if existing.pattern != service.pattern
                || existing.realtime_scope != service.realtime_scope
            {
                return Err(DataError::ConflictingIdentity {
                    path: path.into(),
                    id: key,
                    detail: "TransitService patterns or realtime scopes disagree".into(),
                });
            }
        } else {
            self.services.insert(key, service);
        }
        Ok(true)
    }

    fn skip_service(&mut self, service_id: &TransitServiceId, reason: &str, stops: usize) {
        let key = service_id.as_str().to_owned();
        if self.skipped_services.insert(key.clone()) {
            self.diagnostics.skipped_services.push(SkippedService {
                service_id: key.clone(),
                reason: reason.into(),
                stops,
            });
        }
        self.services.remove(&key);
        self.trips.retain(|_, trip| trip.service_id.as_str() != key);
    }

    fn insert_trip(&mut self, trip: ScheduledTrip, path: &Path) -> Result<(), DataError> {
        if self.skipped_services.contains(trip.service_id.as_str()) {
            return Ok(());
        }
        let key = trip.id.as_str().to_owned();
        if let Some(existing) = self.trips.get(&key) {
            if existing != &trip {
                return Err(DataError::ConflictingIdentity {
                    path: path.into(),
                    id: key,
                    detail: "ScheduledTrip records disagree".into(),
                });
            }
        } else {
            self.trips.insert(key, trip);
        }
        Ok(())
    }

    fn normalize_transfer(&mut self, record: &Value, path: &Path) -> Result<(), DataError> {
        let from = string(record, "FromStopUID").or_else(|| string(record, "from"));
        let to = string(record, "ToStopUID").or_else(|| string(record, "to"));
        let duration = number(record, "DurationSeconds")
            .or_else(|| number(record, "duration_seconds"))
            .and_then(|value| u32::try_from(value).ok());
        if let (Some(from), Some(to), Some(duration)) = (from, to, duration) {
            let from: String = from.into();
            let to: String = to.into();
            self.transfer_edges
                .insert((from.clone(), to.clone(), duration));
            if from != to {
                self.transfer_edges.insert((to, from, duration));
            }
        } else if record.get("FromStopUID").is_some() || record.get("from").is_some() {
            return Err(malformed(
                path,
                "transfer edge",
                "missing endpoint or duration",
            ));
        }
        Ok(())
    }

    fn normalize_metro_line_transfer(
        &mut self,
        record: &Value,
        path: &Path,
    ) -> Result<(), DataError> {
        let from = string(record, "FromStationUID").or_else(|| string(record, "FromStationID"));
        let to = string(record, "ToStationUID").or_else(|| string(record, "ToStationID"));
        let duration = number(record, "TransferTime")
            .and_then(|minutes| minutes.checked_mul(60))
            .and_then(|seconds| u32::try_from(seconds).ok());
        if let (Some(from), Some(to), Some(duration)) = (from, to, duration) {
            let from = self.canonical_metro_stop_id(from);
            let to = self.canonical_metro_stop_id(to);
            // TDX also publishes inter-operator interchange records from this
            // endpoint. Keep only edges whose boarding points belong to the
            // station catalogs included in the selected static scope.
            if from != to && self.stops.contains_key(&from) && self.stops.contains_key(&to) {
                self.transfer_edges.insert((from, to, duration));
            }
        } else if from.is_some() || to.is_some() || record.get("TransferTime").is_some() {
            return Err(malformed(
                path,
                "metro line transfer",
                "missing endpoint or positive duration",
            ));
        }
        Ok(())
    }

    fn validate_diagnostics(&self) -> Result<(), DataError> {
        const MIN_SYSTEMIC_BUS_RECORDS: usize = 8;
        const SYSTEMIC_BUS_RECORD_RATIO: usize = 4;

        let total = self.bus_service_records;
        let affected = self.bus_affected_service_ids.len();
        let malformed = self.diagnostics.malformed_bus_records;
        let systemic = total > 0
            && (affected == total
                || malformed >= total
                || (affected >= MIN_SYSTEMIC_BUS_RECORDS
                    && affected * SYSTEMIC_BUS_RECORD_RATIO >= total)
                || (malformed >= MIN_SYSTEMIC_BUS_RECORDS
                    && malformed * SYSTEMIC_BUS_RECORD_RATIO >= total));
        if systemic {
            return Err(DataError::MalformedSource {
                path: self
                    .bus_source_path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("bus/stop-of-route.json")),
                kind: "bus normalization",
                detail: format!(
                    "systemic schema mismatch: {affected} of {total} service records were degenerate or had unusable stops; malformed records={malformed}"
                ),
            });
        }
        Ok(())
    }

    fn ordered_metro_pattern(&self, stations: &[(u32, BoardingPointId)]) -> Vec<BoardingPointId> {
        let mut ordered = stations
            .iter()
            .map(|(sequence, station_id)| {
                (
                    *sequence,
                    BoardingPointId::new(self.canonical_metro_stop_id(station_id.as_str())),
                )
            })
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(sequence, _)| *sequence);
        ordered.dedup_by(|left, right| left.1 == right.1);
        ordered
            .into_iter()
            .map(|(_, station_id)| station_id)
            .collect()
    }

    fn normalized_metro_path(&self, path: &[BoardingPointId]) -> Vec<BoardingPointId> {
        let mut normalized = Vec::with_capacity(path.len());
        for station_id in path {
            let station_id =
                BoardingPointId::new(self.canonical_metro_stop_id(station_id.as_str()));
            if normalized.last() != Some(&station_id) {
                normalized.push(station_id);
            }
        }
        normalized
    }

    fn longest_metro_route_path(&self, line_id: &str) -> Option<Vec<BoardingPointId>> {
        self.metro_route_paths
            .iter()
            .filter(|((line, _), path)| line == line_id && path.len() >= 2)
            .max_by_key(|(_, path)| path.len())
            .map(|(_, path)| self.normalized_metro_path(path))
    }

    fn orient_metro_path(
        &self,
        path: Vec<BoardingPointId>,
        canonical: &[BoardingPointId],
    ) -> Vec<BoardingPointId> {
        let positions = canonical
            .iter()
            .enumerate()
            .map(|(index, station_id)| (station_id.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        match (
            path.first()
                .and_then(|station| positions.get(station.as_str())),
            path.last()
                .and_then(|station| positions.get(station.as_str())),
        ) {
            (Some(first), Some(last)) if first > last => path.into_iter().rev().collect(),
            _ => path,
        }
    }

    fn metro_headsign(
        &self,
        line_id: &str,
        direction: u8,
        pattern: &[BoardingPointId],
        fallback: &str,
    ) -> String {
        self.metro_headsigns
            .iter()
            .find(|((line, record_direction, destination), headsign)| {
                line == line_id
                    && *record_direction == Some(direction)
                    && pattern.last().is_some_and(|last| {
                        self.canonical_metro_stop_id(destination) == last.as_str()
                    })
                    && !headsign.trim().is_empty()
            })
            .map(|(_, headsign)| headsign.clone())
            .or_else(|| {
                pattern
                    .last()
                    .and_then(|station_id| self.stops.get(station_id.as_str()))
                    .map(|station| station.name.clone())
            })
            .unwrap_or_else(|| fallback.into())
    }

    fn finish(mut self) -> (TransitNetwork, BuildDiagnostics) {
        let mut directional_by_base: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for service_id in self.services.keys() {
            if let Some((base, _)) = service_id.split_once("::direction=") {
                directional_by_base
                    .entry(base.into())
                    .or_default()
                    .push(service_id.clone());
            }
        }
        for (base, directional) in directional_by_base {
            if directional.len() != 1 {
                continue;
            }
            let directional_id = &directional[0];
            let Some(base_service) = self.services.get(&base).cloned() else {
                continue;
            };
            let Some(directional_service) = self.services.get(directional_id) else {
                continue;
            };
            if base_service.pattern != directional_service.pattern {
                continue;
            }
            let directional_service_id = directional_service.id.clone();
            self.services.remove(&base);
            for trip in self.trips.values_mut() {
                if trip.service_id.as_str() == base {
                    trip.service_id = directional_service_id.clone();
                }
            }
        }
        let mut metro_timings = Vec::new();
        let metro_patterns = std::mem::take(&mut self.metro_patterns);
        let mut patterns_by_line = BTreeMap::<String, Vec<MetroPattern>>::new();
        for (_, pattern) in metro_patterns {
            patterns_by_line
                .entry(pattern.line_id.clone())
                .or_default()
                .push(pattern);
        }
        for (line_id, patterns) in patterns_by_line {
            let canonical = patterns
                .iter()
                .find(|pattern| pattern.direction.is_none())
                .map(|pattern| self.ordered_metro_pattern(&pattern.stations))
                .unwrap_or_default();
            let topology = self
                .longest_metro_route_path(&line_id)
                .or_else(|| (!canonical.is_empty()).then(|| canonical.clone()))
                .map(|path| self.orient_metro_path(path, &canonical));

            let mut directional = BTreeMap::<u8, MetroPattern>::new();
            for pattern in patterns
                .iter()
                .filter(|pattern| pattern.direction.is_some())
            {
                let Some(direction) = pattern.direction else {
                    continue;
                };
                let ordered = self.ordered_metro_pattern(&pattern.stations);
                if ordered.len() < 2 {
                    continue;
                }
                let mut pattern = pattern.clone();
                pattern.direction = Some(direction);
                pattern.stations = ordered
                    .into_iter()
                    .enumerate()
                    .map(|(sequence, station_id)| (sequence as u32, station_id))
                    .collect();
                directional.insert(direction, pattern);
            }

            match directional.len() {
                0 => {
                    let Some(topology) = topology else {
                        continue;
                    };
                    for direction in [0, 1] {
                        let pattern = if direction == 0 {
                            topology.clone()
                        } else {
                            topology.iter().cloned().rev().collect()
                        };
                        directional.insert(
                            direction,
                            MetroPattern {
                                line_id: line_id.clone(),
                                direction: Some(direction),
                                label: patterns
                                    .first()
                                    .map(|pattern| pattern.label.clone())
                                    .unwrap_or_else(|| line_id.clone()),
                                headsign: String::new(),
                                stations: pattern
                                    .into_iter()
                                    .enumerate()
                                    .map(|(sequence, station_id)| (sequence as u32, station_id))
                                    .collect(),
                            },
                        );
                    }
                }
                1 => {
                    let (&direction, pattern) = directional.iter().next().expect("one pattern");
                    let pattern = pattern.clone();
                    let opposite = 1_u8.saturating_sub(direction);
                    let opposite_pattern = pattern
                        .stations
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(sequence, (_, station_id))| (sequence as u32, station_id.clone()))
                        .collect();
                    directional.insert(
                        opposite,
                        MetroPattern {
                            line_id: line_id.clone(),
                            direction: Some(opposite),
                            label: pattern.label.clone(),
                            headsign: String::new(),
                            stations: opposite_pattern,
                        },
                    );
                }
                _ => {}
            }

            for (direction, metro_pattern) in directional {
                let pattern = metro_pattern
                    .stations
                    .iter()
                    .map(|(_, station_id)| station_id.clone())
                    .collect::<Vec<_>>();
                if pattern.len() < 2 {
                    continue;
                }
                let service_id =
                    TransitServiceId::new(format!("metro:{line_id}::direction={direction}"));
                let station_timings =
                    self.metro_station_timings(&line_id, Some(direction), &pattern);
                let travel_seconds = self.metro_travel_seconds(&line_id, Some(direction), &pattern);
                let source = if station_timings
                    .iter()
                    .any(|station| !station.departures_seconds.is_empty())
                {
                    "metro_schedule"
                } else if station_timings
                    .iter()
                    .any(|station| !station.frequencies.is_empty())
                {
                    "metro_frequency"
                } else {
                    "metro_schedule"
                };
                let headsign =
                    self.metro_headsign(&line_id, direction, &pattern, &metro_pattern.headsign);
                self.services
                    .entry(service_id.as_str().into())
                    .and_modify(|service| {
                        service.timing_source = Some(source.into());
                        service.headsign = headsign.clone();
                        service.pattern = pattern.clone();
                    })
                    .or_insert_with(|| {
                        TransitService::new(
                            service_id.clone(),
                            TransitMode::Metro,
                            metro_pattern.label.clone(),
                            headsign,
                            pattern.clone(),
                        )
                        .with_timing_source(source)
                    });
                if travel_seconds.len() == pattern.len() - 1
                    && travel_seconds.iter().all(|seconds| *seconds > 0)
                {
                    metro_timings.push(
                        MetroTiming::new(service_id, station_timings, travel_seconds)
                            .with_pattern(pattern),
                    );
                }
            }
        }
        let network = TransitNetwork::new(
            default_service_day_boundary(),
            self.stops.into_values().collect(),
            self.services.into_values().collect(),
            self.trips.into_values().collect(),
            self.bike_stations.into_values().collect(),
        )
        .with_metro_timings(metro_timings)
        .with_transfer_edges(
            self.transfer_edges
                .into_iter()
                .map(|(from, to, duration)| {
                    TransferEdge::new(
                        BoardingPointId::new(from),
                        BoardingPointId::new(to),
                        duration,
                    )
                })
                .collect(),
        );
        (network, self.diagnostics)
    }
}

pub fn build_from_raw(
    input: &Path,
    output: &Path,
    scope: Option<DataScope>,
    raw_snapshot_date: Option<String>,
) -> Result<BuildOutput, DataError> {
    let raw_manifest = read_raw_manifest(input);
    let source_scope = scope.or_else(|| {
        raw_manifest
            .as_ref()
            .and_then(|manifest| DataScope::from_str(&manifest.scope).ok())
    });
    let realtime_scopes = source_scope
        .map(|scope| {
            static_sources(scope)
                .into_iter()
                .map(|source| (source.output_path, source.realtime_scope))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let documents = read_documents(input, &realtime_scopes)?;
    if documents.is_empty() {
        return Err(DataError::EmptySnapshot(input.into()));
    }
    let mut normalizer = Normalizer::default();
    for document in documents
        .iter()
        .filter(|document| is_station_catalog_document(&document.relative_path))
    {
        normalizer.normalize_document(document)?;
    }
    for document in documents
        .iter()
        .filter(|document| is_bus_stop_of_route_document(&document.relative_path))
    {
        normalizer.normalize_document(document)?;
    }
    for document in documents.iter().filter(|document| {
        !is_station_catalog_document(&document.relative_path)
            && !is_bus_stop_of_route_document(&document.relative_path)
    }) {
        normalizer.normalize_document(document)?;
    }
    normalizer.validate_diagnostics()?;
    let (mut network, diagnostics) = normalizer.finish();
    network = network.canonicalize();
    if network.stops.is_empty() {
        return Err(DataError::EmptyNetwork);
    }
    let static_revision = revision_for_network(&network, false)?;
    let schedule_revision = revision_for_network(&network, true)?;
    network = network.with_revisions(static_revision.clone(), schedule_revision.clone());
    network.validate()?;
    let transfer_index = TransferShortcutIndex::from_network(&network);

    let sources = documents
        .iter()
        .map(|document| {
            (
                document.relative_path.to_string_lossy().into_owned(),
                sha256_hex(&document.bytes),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let manifest_scope = scope
        .map(|scope| scope.as_str().to_owned())
        .or_else(|| raw_manifest.as_ref().map(|manifest| manifest.scope.clone()))
        .unwrap_or_else(|| "unknown".into());
    let raw_snapshot_date =
        raw_snapshot_date.or_else(|| raw_manifest.map(|manifest| manifest.date));
    let manifest = ArtifactManifest {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        scope: manifest_scope,
        built_at: time::OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".into()),
        raw_snapshot_date,
        sources,
        counts: ArtifactCounts {
            stops: network.stops.len(),
            services: network.services.len(),
            scheduled_trips: network.trips.len(),
            bike_stations: network.bike_share_stations.len(),
            transfer_edges: transfer_index.len(),
        },
        revisions: ArtifactRevisions {
            static_network_revision: static_revision,
            schedule_revision,
        },
    };
    write_network_artifact(output, &network)?;
    write_json_atomic(&output.join("manifest.json"), &manifest)?;
    Ok(BuildOutput {
        network,
        manifest,
        transfer_index,
        diagnostics,
    })
}

pub fn load_network_artifact(path: &Path) -> Result<TransitNetwork, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    TransitNetwork::from_artifact_json(&body)
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))
}

pub fn load_manifest(path: &Path) -> Result<ArtifactManifest, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    serde_json::from_str(&body).map_err(|error| DataError::Json(path.into(), error))
}

pub fn write_network_artifact(output: &Path, network: &TransitNetwork) -> Result<(), DataError> {
    let body = serde_json::to_string_pretty(&network.clone().canonicalize())
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
    write_text_atomic(&output.join("transit-network.json"), &body)
}

pub async fn fetch_raw_snapshot(
    scope: DataScope,
    date: Date,
    output: &Path,
    credentials: TdxCredentials,
) -> Result<RawSnapshotManifest, DataError> {
    Ok(
        fetch_raw_snapshot_with_progress(scope, date, output, credentials, |_| {})
            .await?
            .manifest,
    )
}

pub async fn fetch_raw_snapshot_with_progress<F>(
    scope: DataScope,
    date: Date,
    output: &Path,
    credentials: TdxCredentials,
    progress: F,
) -> Result<RawSnapshotFetchReport, DataError>
where
    F: FnMut(FetchProgress),
{
    let api_base = std::env::var("TDX_API_BASE_URL")
        .or_else(|_| std::env::var("VIA_TDX_BASE_URL"))
        .unwrap_or_else(|_| "https://tdx.transportdata.tw/api/basic/".into());
    let token_url = std::env::var("TDX_TOKEN_URL").unwrap_or_else(|_| {
        "https://tdx.transportdata.tw/auth/realms/TDXConnect/protocol/openid-connect/token".into()
    });
    let client = TdxClient::with_urls(credentials, &api_base, &token_url).map_err(|error| {
        DataError::Fetch {
            source_name: "TDX client".into(),
            error,
        }
    })?;
    let mut sleeper = TokioFetchSleeper;
    let mut progress = progress;
    fetch_raw_snapshot_with_client(
        scope,
        date,
        output,
        &client,
        FetchTimingPolicy::default(),
        &mut sleeper,
        &mut progress,
    )
    .await
}

#[derive(Clone, Copy)]
struct FetchTimingPolicy {
    request_spacing: Duration,
    retry_backoffs: [Duration; 4],
}

impl Default for FetchTimingPolicy {
    fn default() -> Self {
        Self {
            request_spacing: TDX_REQUEST_SPACING,
            retry_backoffs: TDX_RETRY_BACKOFFS,
        }
    }
}

#[async_trait::async_trait]
trait FetchSleeper {
    async fn sleep(&mut self, duration: Duration);
}

struct TokioFetchSleeper;

#[async_trait::async_trait]
impl FetchSleeper for TokioFetchSleeper {
    async fn sleep(&mut self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

struct TdxRequestLimiter {
    last_request_at: Option<Instant>,
    spacing: Duration,
}

impl TdxRequestLimiter {
    fn new(spacing: Duration) -> Self {
        Self {
            last_request_at: None,
            spacing,
        }
    }

    async fn wait<S: FetchSleeper>(
        &mut self,
        sleeper: &mut S,
        progress: &mut impl FnMut(FetchProgress),
    ) {
        if let Some(last_request_at) = self.last_request_at {
            let elapsed = last_request_at.elapsed();
            if elapsed < self.spacing {
                let duration = self.spacing - elapsed;
                progress(FetchProgress::Wait { duration });
                sleeper.sleep(duration).await;
            }
        }
        self.last_request_at = Some(Instant::now());
    }
}

async fn fetch_raw_snapshot_with_client<S, F>(
    scope: DataScope,
    date: Date,
    output: &Path,
    client: &TdxClient,
    timing: FetchTimingPolicy,
    sleeper: &mut S,
    progress: &mut F,
) -> Result<RawSnapshotFetchReport, DataError>
where
    S: FetchSleeper,
    F: FnMut(FetchProgress),
{
    let sources = static_sources(scope);
    let snapshot = output.join("raw").join(date.to_string());
    fs::create_dir_all(&snapshot).map_err(|error| DataError::Io(snapshot.clone(), error))?;
    let previous_manifest = read_previous_raw_manifest(&snapshot, scope, date);
    let mut metadata = BTreeMap::new();
    let mut limiter = TdxRequestLimiter::new(timing.request_spacing);
    let mut reused_count = 0;
    let mut downloaded_count = 0;

    for source in sources {
        let path = snapshot.join(&source.output_path);
        if let Some(body) = reusable_existing_json(&path, &source, previous_manifest.as_ref()) {
            progress(FetchProgress::Reuse {
                source_key: source.key.clone(),
                relative_path: source.output_path.to_string_lossy().into_owned(),
            });
            metadata.insert(
                source.key.clone(),
                raw_source_metadata(&source, &body, previous_manifest.as_ref()),
            );
            reused_count += 1;
            continue;
        }

        progress(FetchProgress::Fetch {
            source_key: source.key.clone(),
        });
        let response =
            fetch_static_source(client, &source, &mut limiter, timing, sleeper, progress)
                .await
                .map_err(|error| DataError::Fetch {
                    source_name: source.key.clone(),
                    error,
                })?;
        serde_json::from_slice::<Value>(&response.body)
            .map_err(|error| DataError::Json(path.clone(), error))?;
        write_bytes_atomic(&path, &response.body)?;
        metadata.insert(
            source.key.clone(),
            raw_source_metadata(&source, &response.body, None),
        );
        downloaded_count += 1;
    }
    let manifest = RawSnapshotManifest {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        scope: scope.as_str().into(),
        date: date.to_string(),
        sources: metadata,
    };
    write_json_atomic(&snapshot.join("manifest.json"), &manifest)?;
    Ok(RawSnapshotFetchReport {
        manifest,
        reused_count,
        downloaded_count,
    })
}

async fn fetch_static_source<S, F>(
    client: &TdxClient,
    source: &StaticSource,
    limiter: &mut TdxRequestLimiter,
    timing: FetchTimingPolicy,
    sleeper: &mut S,
    progress: &mut F,
) -> Result<crate::config::TdxRawResponse, TdxClientError>
where
    S: FetchSleeper,
    F: FnMut(FetchProgress),
{
    for retry_index in 0..=timing.retry_backoffs.len() {
        limiter.wait(sleeper, progress).await;
        let query = source
            .query
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        match client.get_raw(&source.path, &query).await {
            Ok(response) => return Ok(response),
            Err(error)
                if matches!(
                    error,
                    TdxClientError::RateLimited | TdxClientError::RateLimitedWithRetryAfter(_)
                ) =>
            {
                let Some(default_retry_delay) = timing.retry_backoffs.get(retry_index).copied()
                else {
                    return Err(TdxClientError::RateLimited);
                };
                let retry_delay = error.retry_after().unwrap_or(default_retry_delay);
                progress(FetchProgress::Retry {
                    source_key: source.key.clone(),
                    duration: retry_delay,
                });
                sleeper.sleep(retry_delay).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded retry loop always returns")
}

fn valid_existing_json(path: &Path) -> Option<Vec<u8>> {
    let body = fs::read(path).ok()?;
    if body.is_empty() || serde_json::from_slice::<Value>(&body).is_err() {
        return None;
    }
    Some(body)
}

fn reusable_existing_json(
    path: &Path,
    source: &StaticSource,
    previous_manifest: Option<&RawSnapshotManifest>,
) -> Option<Vec<u8>> {
    let body = valid_existing_json(path)?;
    let Some(previous_manifest) = previous_manifest else {
        return Some(body);
    };
    let previous = previous_manifest.sources.get(&source.key)?;
    (previous.endpoint == source.endpoint
        && previous.path == source.output_path.to_string_lossy()
        && previous.sha256 == sha256_hex(&body))
    .then_some(body)
}

fn read_previous_raw_manifest(
    snapshot: &Path,
    scope: DataScope,
    date: Date,
) -> Option<RawSnapshotManifest> {
    let body = fs::read_to_string(snapshot.join("manifest.json")).ok()?;
    let manifest = serde_json::from_str::<RawSnapshotManifest>(&body).ok()?;
    (manifest.scope == scope.as_str() && manifest.date == date.to_string()).then_some(manifest)
}

fn raw_source_metadata(
    source: &StaticSource,
    body: &[u8],
    previous_manifest: Option<&RawSnapshotManifest>,
) -> RawSourceMetadata {
    let path = source.output_path.to_string_lossy().into_owned();
    let sha256 = sha256_hex(body);
    let retrieved_at = previous_manifest
        .and_then(|manifest| manifest.sources.get(&source.key))
        .filter(|previous| {
            previous.endpoint == source.endpoint
                && previous.path == path
                && previous.sha256 == sha256
                && time::OffsetDateTime::parse(&previous.retrieved_at, &Rfc3339).is_ok()
        })
        .map(|previous| previous.retrieved_at.clone())
        .unwrap_or_else(current_retrieved_at);
    RawSourceMetadata {
        endpoint: source.endpoint.clone(),
        path,
        sha256,
        retrieved_at,
    }
}

fn current_retrieved_at() -> String {
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

fn static_sources(scope: DataScope) -> Vec<StaticSource> {
    let mut sources = Vec::new();
    for (city_key, city) in scope.cities() {
        for (name, endpoint) in [
            ("route", format!("v2/Bus/Route/City/{city}")),
            ("stop", format!("v2/Bus/Stop/City/{city}")),
            ("stop-of-route", format!("v2/Bus/StopOfRoute/City/{city}")),
            ("schedule", format!("v2/Bus/Schedule/City/{city}")),
        ] {
            sources.push(source_with_realtime_scope(
                format!("tdx_bus_{city_key}_{name}"),
                endpoint,
                format!("bus/{city_key}/{name}.json"),
                city,
            ));
        }
        sources.push(source(
            format!("tdx_youbike_{city_key}_station"),
            format!("v2/Bike/Station/City/{city}"),
            format!("bike/{city_key}/station.json"),
        ));
    }
    sources.extend([
        source(
            "tdx_metro_station",
            "v2/Rail/Metro/Station/TRTC".into(),
            "metro/trtc-station.json".into(),
        ),
        source(
            "tdx_metro_station_of_line",
            "v2/Rail/Metro/StationOfLine/TRTC".into(),
            "metro/trtc-station-of-line.json".into(),
        ),
        source(
            "tdx_metro_line_transfer",
            "v2/Rail/Metro/LineTransfer/TRTC".into(),
            "metro/trtc-line-transfer.json".into(),
        ),
        source(
            "tdx_metro_station_timetable",
            "v2/Rail/Metro/StationTimeTable/TRTC".into(),
            "metro/trtc-station-timetable.json".into(),
        ),
        source(
            "tdx_metro_s2s_travel_time",
            "v2/Rail/Metro/S2STravelTime/TRTC".into(),
            "metro/trtc-s2s-travel-time.json".into(),
        ),
        source(
            "tdx_metro_frequency",
            "v2/Rail/Metro/Frequency/TRTC".into(),
            "metro/trtc-frequency.json".into(),
        ),
        source(
            "tdx_tra_station",
            "v3/Rail/TRA/Station".into(),
            "tra/station.json".into(),
        ),
        source(
            "tdx_tra_general_train_timetable",
            "v3/Rail/TRA/GeneralTrainTimetable".into(),
            "tra/general-timetable.json".into(),
        ),
        source(
            "tdx_thsr_station",
            "v2/Rail/THSR/Station".into(),
            "thsr/station.json".into(),
        ),
        source(
            "tdx_thsr_general_timetable",
            "v2/Rail/THSR/GeneralTimetable".into(),
            "thsr/general-timetable.json".into(),
        ),
    ]);
    sources
}

fn source(key: impl Into<String>, path: String, output_path: String) -> StaticSource {
    let query = vec![("$format".into(), "JSON".into())];
    StaticSource {
        key: key.into(),
        endpoint: format!("{path}?$format=JSON"),
        path,
        query,
        output_path: output_path.into(),
        realtime_scope: None,
    }
}

fn source_with_realtime_scope(
    key: impl Into<String>,
    path: String,
    output_path: String,
    realtime_scope: &str,
) -> StaticSource {
    let mut source = source(key, path, output_path);
    source.realtime_scope = Some(realtime_scope.into());
    source
}

fn read_raw_manifest(input: &Path) -> Option<RawSnapshotManifest> {
    let body = fs::read_to_string(input.join("manifest.json")).ok()?;
    serde_json::from_str(&body).ok()
}

fn read_documents(
    input: &Path,
    realtime_scopes: &BTreeMap<PathBuf, Option<String>>,
) -> Result<Vec<RawDocument>, DataError> {
    let mut paths = Vec::new();
    collect_json_paths(input, &mut paths)?;
    paths.sort();
    paths.retain(|path| path.file_name().is_none_or(|name| name != "manifest.json"));
    paths
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).map_err(|error| DataError::Io(path.clone(), error))?;
            let value = serde_json::from_slice(&bytes)
                .map_err(|error| DataError::Json(path.clone(), error))?;
            let relative_path = path.strip_prefix(input).unwrap_or(&path).to_path_buf();
            let realtime_scope = realtime_scopes.get(&relative_path).cloned().flatten();
            Ok(RawDocument {
                relative_path,
                bytes,
                value,
                realtime_scope,
            })
        })
        .collect()
}

fn collect_json_paths(path: &Path, output: &mut Vec<PathBuf>) -> Result<(), DataError> {
    let metadata = fs::metadata(path).map_err(|error| DataError::Io(path.into(), error))?;
    if metadata.is_file() {
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            output.push(path.to_path_buf());
        }
        return Ok(());
    }
    let mut entries = fs::read_dir(path)
        .map_err(|error| DataError::Io(path.into(), error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| DataError::Io(path.into(), error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        collect_json_paths(&entry.path(), output)?;
    }
    Ok(())
}

fn is_station_catalog_document(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name == "station.json" || name == "stop.json" || name == "trtc-station.json"
    })
}

fn is_bus_stop_catalog_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "stop.json")
}

fn is_bus_stop_of_route_document(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "stop-of-route.json")
}

fn records(value: &Value) -> Vec<&Value> {
    if let Some(array) = value.as_array() {
        return array.iter().collect();
    }
    if let Some(array) = value.get("value").and_then(Value::as_array) {
        return array.iter().collect();
    }
    for key in ["Stations", "TrainTimetables", "shortcuts"] {
        if let Some(array) = value.get(key).and_then(Value::as_array) {
            return array.iter().collect();
        }
    }
    vec![value]
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn metro_line_id(value: &Value) -> Option<String> {
    ["LineID", "LineUID", "RouteUID", "RouteID"]
        .iter()
        .find_map(|key| string(value, key).map(str::to_owned))
}

fn metro_route_id(value: &Value) -> Option<String> {
    ["RouteID", "RouteUID", "LineID", "LineUID"]
        .iter()
        .find_map(|key| string(value, key).map(str::to_owned))
}

fn is_metro_station_timetable_path(path: &str) -> bool {
    path.contains("station-timetable")
        || path.contains("stationtimetable")
        || path.contains("station_time_table")
}

fn is_rail_path(path: &str) -> bool {
    path.contains("metro")
        || path.contains("rail")
        || path.contains("tra/")
        || path.contains("thsr/")
}

fn metro_direction(value: &Value) -> Option<u8> {
    number(value, "Direction")
        .or_else(|| string(value, "Direction").and_then(|value| value.parse().ok()))
        .and_then(|value| u8::try_from(value).ok())
}

fn metro_station_id(normalizer: &Normalizer, value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let station = string(value, key)?;
        if key.ends_with("UID") {
            return Some(boarding_point_id(station).as_str().to_owned());
        }
        normalizer
            .station_aliases
            .get(&format!("Metro:{station}"))
            .or_else(|| normalizer.station_aliases.get(station))
            .map(|id| id.as_str().to_owned())
            .or_else(|| Some(boarding_point_id(station).as_str().to_owned()))
    })
}

fn metro_duration_seconds(value: &Value) -> Option<u32> {
    [
        "TravelTimeSeconds",
        "TravelTime",
        "RunTimeSeconds",
        "RunTime",
        "DurationSeconds",
        "Duration",
        "Seconds",
    ]
    .iter()
    .find_map(|key| {
        value.get(*key).and_then(|value| {
            value
                .as_u64()
                .and_then(|seconds| u32::try_from(seconds).ok())
                .or_else(|| value.as_str().and_then(|value| time_seconds(Some(value))))
        })
    })
    .or_else(|| {
        let minutes = number(value, "Minutes").or_else(|| number(value, "Min"))?;
        let seconds = number(value, "Seconds")
            .or_else(|| number(value, "Sec"))
            .unwrap_or(0);
        u32::try_from(minutes.checked_mul(60)?.checked_add(seconds)?).ok()
    })
}

fn metro_frequency(value: &Value) -> Option<(u32, u32, u32)> {
    let start_seconds = ["StartTime", "Start", "FromTime"]
        .iter()
        .find_map(|key| value.get(*key).and_then(time_seconds_value))?;
    let end_seconds = ["EndTime", "End", "ToTime"]
        .iter()
        .find_map(|key| value.get(*key).and_then(time_seconds_value))?;
    let headway_seconds = [
        "HeadwaySeconds",
        "IntervalSeconds",
        "FrequencySeconds",
        "Headway",
        "Interval",
        "Frequency",
    ]
    .iter()
    .find_map(|key| value.get(*key).and_then(time_seconds_value))
    .or_else(|| {
        [
            "PeakHeadway",
            "OffPeakHeadway",
            "MinHeadwayMins",
            "MaxHeadwayMins",
        ]
        .iter()
        .find_map(|key| {
            number(value, key)
                .and_then(|minutes| minutes.checked_mul(60))
                .and_then(|seconds| u32::try_from(seconds).ok())
        })
    })
    .or_else(|| {
        let minutes = number(value, "Minutes").or_else(|| number(value, "Min"))?;
        let seconds = number(value, "Seconds")
            .or_else(|| number(value, "Sec"))
            .unwrap_or(0);
        u32::try_from(minutes.checked_mul(60)?.checked_add(seconds)?).ok()
    })?;
    (start_seconds <= end_seconds && headway_seconds > 0).then_some((
        start_seconds,
        end_seconds,
        headway_seconds,
    ))
}

fn display_name(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                value
                    .get("Zh_cn")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .get("Zh_tw")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .get("zh_tw")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
    })
}

fn coordinate(value: &Value) -> Option<crate::api::RequestCoordinate> {
    let position = value
        .get("StopPosition")
        .or_else(|| value.get("StationPosition"))
        .or_else(|| value.get("Position"))
        .unwrap_or(value);
    let latitude = position
        .get("PositionLat")
        .or_else(|| position.get("latitude"))
        .and_then(Value::as_f64)
        .or_else(|| value.get("PositionLat").and_then(Value::as_f64))?;
    let longitude = position
        .get("PositionLon")
        .or_else(|| position.get("longitude"))
        .and_then(Value::as_f64)
        .or_else(|| value.get("PositionLon").and_then(Value::as_f64))?;
    Some(crate::api::RequestCoordinate {
        latitude,
        longitude,
    })
}

fn time_seconds(value: Option<&str>) -> Option<u32> {
    let value = value?.trim();
    if let Ok(seconds) = value.parse::<u32>() {
        return Some(seconds);
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let hour = parts[0].parse::<u32>().ok()?;
    let minute = parts[1].parse::<u32>().ok()?;
    let second = parts
        .get(2)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    (minute < 60 && second < 60).then(|| hour * 3_600 + minute * 60 + second)
}

fn lift_time_after(time: u32, lower_bound: u32) -> Option<u32> {
    let mut lifted = time;
    while lifted < lower_bound {
        lifted = lifted.checked_add(SECONDS_PER_SERVICE_DAY)?;
    }
    Some(lifted)
}

fn time_seconds_value(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|seconds| u32::try_from(seconds).ok())
        .or_else(|| value.as_str().and_then(|value| time_seconds(Some(value))))
}

fn distinct_stop_count(pattern: &[BoardingPointId]) -> usize {
    pattern
        .iter()
        .map(BoardingPointId::as_str)
        .collect::<BTreeSet<_>>()
        .len()
}

fn service_calendar(value: &Value) -> (Vec<Weekday>, Vec<Date>, Vec<Date>) {
    let active = value
        .get("ActiveWeekdays")
        .or_else(|| value.get("OperatingDays"))
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(parse_weekday).collect::<Vec<_>>())
        .or_else(|| {
            let service_day = value.get("ServiceDay")?.as_object()?;
            Some(
                [
                    ("Monday", Weekday::Monday),
                    ("Tuesday", Weekday::Tuesday),
                    ("Wednesday", Weekday::Wednesday),
                    ("Thursday", Weekday::Thursday),
                    ("Friday", Weekday::Friday),
                    ("Saturday", Weekday::Saturday),
                    ("Sunday", Weekday::Sunday),
                ]
                .into_iter()
                .filter_map(|(key, weekday)| {
                    service_day_active(service_day, key).then_some(weekday)
                })
                .collect(),
            )
        })
        .filter(|days| !days.is_empty() || value.get("ServiceDay").is_some())
        .unwrap_or_else(|| ALL_WEEKDAYS.to_vec());
    let mut added: Vec<Date> = value
        .get("AddedServiceDays")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().and_then(parse_date))
                .collect()
        })
        .unwrap_or_default();
    let mut removed: Vec<Date> = value
        .get("RemovedServiceDays")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().and_then(parse_date))
                .collect()
        })
        .unwrap_or_default();
    if let Some(special_days) = value.get("SpecialDays").and_then(Value::as_array) {
        for special_day in special_days {
            let Some(status) = number(special_day, "ServiceStatus") else {
                continue;
            };
            for date in special_day_dates(special_day) {
                match status {
                    0 => removed.push(date),
                    1 => added.push(date),
                    _ => {}
                }
            }
        }
    }
    (active, added, removed)
}

fn special_day_dates(value: &Value) -> Vec<Date> {
    let mut dates = value
        .get("Dates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(parse_date)
        .collect::<Vec<_>>();
    let Some(period) = value.get("DatePeriod").and_then(Value::as_object) else {
        return dates;
    };
    let (Some(mut date), Some(end)) = (
        period
            .get("StartDate")
            .and_then(Value::as_str)
            .and_then(parse_date),
        period
            .get("EndDate")
            .and_then(Value::as_str)
            .and_then(parse_date),
    ) else {
        return dates;
    };
    while date <= end {
        dates.push(date);
        let Some(next) = date.next_day() else {
            break;
        };
        date = next;
    }
    dates
}

fn service_day_active(service_day: &serde_json::Map<String, Value>, key: &str) -> bool {
    service_day
        .get(key)
        .and_then(|value| {
            value
                .as_u64()
                .map(|value| value != 0)
                .or_else(|| value.as_bool())
        })
        .unwrap_or(false)
}

fn parse_weekday(value: &Value) -> Option<Weekday> {
    let number = value.as_u64()?;
    match number {
        1 => Some(Weekday::Monday),
        2 => Some(Weekday::Tuesday),
        3 => Some(Weekday::Wednesday),
        4 => Some(Weekday::Thursday),
        5 => Some(Weekday::Friday),
        6 => Some(Weekday::Saturday),
        7 => Some(Weekday::Sunday),
        _ => None,
    }
}

fn parse_date(value: &str) -> Option<Date> {
    let format = time::format_description::parse("[year]-[month]-[day]").ok()?;
    Date::parse(value, &format).ok()
}

fn malformed(path: &Path, kind: &'static str, detail: impl Into<String>) -> DataError {
    DataError::MalformedSource {
        path: path.into(),
        kind,
        detail: detail.into(),
    }
}

fn revision_for_network(network: &TransitNetwork, schedule: bool) -> Result<String, DataError> {
    let mut network = network.clone();
    network.static_network_revision.clear();
    network.schedule_revision.clear();
    if schedule {
        network.stops.clear();
        network.services.clear();
        network.bike_share_stations.clear();
        network.transfer_edges.clear();
    } else {
        network.trips.clear();
    }
    let bytes = serde_json::to_vec(&network.canonicalize())
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
    Ok(format!("sha256:{}", sha256_hex(&bytes)))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), DataError> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
    write_text_atomic(path, &body)
}

fn write_text_atomic(path: &Path, body: &str) -> Result<(), DataError> {
    write_bytes_atomic(path, body.as_bytes())
}

fn write_bytes_atomic(path: &Path, body: &[u8]) -> Result<(), DataError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DataError::Io(parent.into(), error))?;
    }
    let temporary = path.with_extension(format!(
        "{}tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
    ));
    fs::write(&temporary, body).map_err(|error| DataError::Io(temporary.clone(), error))?;
    fs::rename(&temporary, path).map_err(|error| DataError::Io(path.into(), error))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{StatusCode, Uri, header::RETRY_AFTER},
        response::{IntoResponse, Response},
        routing::post,
    };
    use serde_json::json;

    use super::*;
    use crate::config::{TdxClient, TdxCredentials};

    #[derive(Clone, Copy)]
    enum MockMode {
        Success,
        RetryAfterThenSuccess,
        AlwaysRateLimited,
        PermanentBadRequest,
    }

    #[derive(Clone)]
    struct MockTdxState {
        mode: MockMode,
        data_calls: Arc<AtomicUsize>,
        token_calls: Arc<AtomicUsize>,
    }

    #[derive(Default)]
    struct RecordingSleeper {
        waits: Vec<Duration>,
    }

    #[async_trait::async_trait]
    impl FetchSleeper for RecordingSleeper {
        async fn sleep(&mut self, duration: Duration) {
            self.waits.push(duration);
        }
    }

    async fn mock_token(State(state): State<MockTdxState>) -> impl IntoResponse {
        state.token_calls.fetch_add(1, Ordering::Relaxed);
        Json(json!({
            "access_token": "test-access-token",
            "expires_in": 3600
        }))
    }

    async fn mock_data(State(state): State<MockTdxState>, uri: Uri) -> Response {
        let call = state.data_calls.fetch_add(1, Ordering::Relaxed);
        let rate_limited = match state.mode {
            MockMode::Success => false,
            MockMode::RetryAfterThenSuccess => call == 0,
            MockMode::AlwaysRateLimited => true,
            MockMode::PermanentBadRequest => false,
        };
        let status = if matches!(state.mode, MockMode::PermanentBadRequest) {
            StatusCode::BAD_REQUEST
        } else if rate_limited {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::OK
        };
        let mut builder = Response::builder().status(status);
        if matches!(state.mode, MockMode::RetryAfterThenSuccess) && call == 0 {
            builder = builder.header(RETRY_AFTER, "7");
        }
        builder
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"path": uri.path(), "query": uri.query(), "call": call}).to_string(),
            ))
            .unwrap()
    }

    async fn mock_client(mode: MockMode) -> (TdxClient, MockTdxState) {
        let state = MockTdxState {
            mode,
            data_calls: Arc::new(AtomicUsize::new(0)),
            token_calls: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/token", post(mock_token))
            .fallback(mock_data)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base_url = format!("http://{address}/api/basic/");
        let token_url = format!("http://{address}/token");
        let client =
            TdxClient::with_urls(TdxCredentials::for_tests(), &base_url, &token_url).unwrap();
        (client, state)
    }

    fn test_date() -> Date {
        Date::from_calendar_date(2026, time::Month::September, 15).unwrap()
    }

    fn temporary_output(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "via-fetch-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn seed_sources(output: &Path, date: Date, skip: &BTreeSet<String>, body: &[u8]) {
        let snapshot = output.join("raw").join(date.to_string());
        for source in static_sources(DataScope::TaipeiNewTaipei) {
            if skip.contains(&source.key) {
                continue;
            }
            let path = snapshot.join(&source.output_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
    }

    fn fast_timing() -> FetchTimingPolicy {
        FetchTimingPolicy {
            request_spacing: Duration::ZERO,
            retry_backoffs: [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(3),
                Duration::from_secs(4),
            ],
        }
    }

    async fn run_fetch(
        output: &Path,
        client: &TdxClient,
        timing: FetchTimingPolicy,
    ) -> (
        Result<RawSnapshotFetchReport, DataError>,
        RecordingSleeper,
        Vec<FetchProgress>,
    ) {
        let mut sleeper = RecordingSleeper::default();
        let mut progress = Vec::new();
        let result = fetch_raw_snapshot_with_client(
            DataScope::TaipeiNewTaipei,
            test_date(),
            output,
            client,
            timing,
            &mut sleeper,
            &mut |event| progress.push(event),
        )
        .await;
        (result, sleeper, progress)
    }

    #[test]
    fn default_fetch_policy_spaces_requests_and_bounds_backoff() {
        let timing = FetchTimingPolicy::default();
        assert_eq!(timing.request_spacing, Duration::from_secs(13));
        assert_eq!(timing.retry_backoffs, TDX_RETRY_BACKOFFS);
    }

    #[test]
    fn static_sources_match_current_tdx_contracts() {
        let sources = static_sources(DataScope::TaipeiNewTaipei);
        assert_eq!(
            sources
                .iter()
                .map(|source| source.endpoint.as_str())
                .collect::<Vec<_>>(),
            static_sources(DataScope::TaipeiNewTaipei)
                .iter()
                .map(|source| source.endpoint.as_str())
                .collect::<Vec<_>>()
        );
        let actual = sources
            .iter()
            .map(|source| {
                (
                    source.key.as_str(),
                    source.path.as_str(),
                    source.output_path.to_string_lossy().into_owned(),
                )
            })
            .collect::<Vec<_>>();
        let expected = vec![
            (
                "tdx_bus_taipei_route",
                "v2/Bus/Route/City/Taipei",
                "bus/taipei/route.json",
            ),
            (
                "tdx_bus_taipei_stop",
                "v2/Bus/Stop/City/Taipei",
                "bus/taipei/stop.json",
            ),
            (
                "tdx_bus_taipei_stop-of-route",
                "v2/Bus/StopOfRoute/City/Taipei",
                "bus/taipei/stop-of-route.json",
            ),
            (
                "tdx_bus_taipei_schedule",
                "v2/Bus/Schedule/City/Taipei",
                "bus/taipei/schedule.json",
            ),
            (
                "tdx_youbike_taipei_station",
                "v2/Bike/Station/City/Taipei",
                "bike/taipei/station.json",
            ),
            (
                "tdx_bus_new-taipei_route",
                "v2/Bus/Route/City/NewTaipei",
                "bus/new-taipei/route.json",
            ),
            (
                "tdx_bus_new-taipei_stop",
                "v2/Bus/Stop/City/NewTaipei",
                "bus/new-taipei/stop.json",
            ),
            (
                "tdx_bus_new-taipei_stop-of-route",
                "v2/Bus/StopOfRoute/City/NewTaipei",
                "bus/new-taipei/stop-of-route.json",
            ),
            (
                "tdx_bus_new-taipei_schedule",
                "v2/Bus/Schedule/City/NewTaipei",
                "bus/new-taipei/schedule.json",
            ),
            (
                "tdx_youbike_new-taipei_station",
                "v2/Bike/Station/City/NewTaipei",
                "bike/new-taipei/station.json",
            ),
            (
                "tdx_metro_station",
                "v2/Rail/Metro/Station/TRTC",
                "metro/trtc-station.json",
            ),
            (
                "tdx_metro_station_of_line",
                "v2/Rail/Metro/StationOfLine/TRTC",
                "metro/trtc-station-of-line.json",
            ),
            (
                "tdx_metro_line_transfer",
                "v2/Rail/Metro/LineTransfer/TRTC",
                "metro/trtc-line-transfer.json",
            ),
            (
                "tdx_metro_station_timetable",
                "v2/Rail/Metro/StationTimeTable/TRTC",
                "metro/trtc-station-timetable.json",
            ),
            (
                "tdx_metro_s2s_travel_time",
                "v2/Rail/Metro/S2STravelTime/TRTC",
                "metro/trtc-s2s-travel-time.json",
            ),
            (
                "tdx_metro_frequency",
                "v2/Rail/Metro/Frequency/TRTC",
                "metro/trtc-frequency.json",
            ),
            ("tdx_tra_station", "v3/Rail/TRA/Station", "tra/station.json"),
            (
                "tdx_tra_general_train_timetable",
                "v3/Rail/TRA/GeneralTrainTimetable",
                "tra/general-timetable.json",
            ),
            (
                "tdx_thsr_station",
                "v2/Rail/THSR/Station",
                "thsr/station.json",
            ),
            (
                "tdx_thsr_general_timetable",
                "v2/Rail/THSR/GeneralTimetable",
                "thsr/general-timetable.json",
            ),
        ]
        .into_iter()
        .map(|(key, path, output)| (key, path, output.to_owned()))
        .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        for source in sources {
            assert_eq!(source.query, [("$format".into(), "JSON".into())]);
            assert_eq!(source.endpoint, format!("{}?$format=JSON", source.path));
        }
    }

    #[tokio::test]
    async fn existing_valid_sources_are_reused_without_data_http_calls() {
        let output = temporary_output("reuse");
        let date = test_date();
        seed_sources(&output, date, &BTreeSet::new(), br#"{"cached":true}"#);
        let (client, state) = mock_client(MockMode::Success).await;

        let (result, sleeper, progress) = run_fetch(&output, &client, fast_timing()).await;
        let report = result.unwrap();

        assert_eq!(report.reused_count, 20);
        assert_eq!(report.downloaded_count, 0);
        assert_eq!(report.manifest.sources.len(), 20);
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 0);
        assert_eq!(state.token_calls.load(Ordering::Relaxed), 0);
        assert!(sleeper.waits.is_empty());
        assert!(
            progress
                .iter()
                .all(|event| matches!(event, FetchProgress::Reuse { .. }))
        );
        assert!(
            output
                .join("raw")
                .join(date.to_string())
                .join("manifest.json")
                .is_file()
        );
    }

    #[tokio::test]
    async fn partial_snapshot_resumes_without_an_existing_manifest() {
        let output = temporary_output("partial");
        let date = test_date();
        let sources = static_sources(DataScope::TaipeiNewTaipei);
        let skip = sources
            .iter()
            .skip(5)
            .map(|source| source.key.clone())
            .collect::<BTreeSet<_>>();
        seed_sources(&output, date, &skip, br#"{"cached":true}"#);
        assert!(
            !output
                .join("raw")
                .join(date.to_string())
                .join("manifest.json")
                .exists()
        );
        let (client, state) = mock_client(MockMode::Success).await;

        let (result, _, _) = run_fetch(&output, &client, fast_timing()).await;
        let report = result.unwrap();

        assert_eq!(report.reused_count, 5);
        assert_eq!(report.downloaded_count, 15);
        assert_eq!(report.manifest.sources.len(), 20);
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 15);
        assert_eq!(state.token_calls.load(Ordering::Relaxed), 1);
        assert!(
            output
                .join("raw")
                .join(date.to_string())
                .join("manifest.json")
                .is_file()
        );
    }

    #[tokio::test]
    async fn empty_and_corrupt_sources_are_refetched() {
        let output = temporary_output("broken");
        let date = test_date();
        let sources = static_sources(DataScope::TaipeiNewTaipei);
        let skip = sources
            .iter()
            .take(2)
            .map(|source| source.key.clone())
            .collect::<BTreeSet<_>>();
        seed_sources(&output, date, &skip, br#"{"cached":true}"#);
        let snapshot = output.join("raw").join(date.to_string());
        fs::write(snapshot.join(&sources[0].output_path), Vec::<u8>::new()).unwrap();
        fs::write(snapshot.join(&sources[1].output_path), b"not-json").unwrap();
        let (client, state) = mock_client(MockMode::Success).await;

        let (result, _, _) = run_fetch(&output, &client, fast_timing()).await;
        let report = result.unwrap();

        assert_eq!(report.reused_count, 18);
        assert_eq!(report.downloaded_count, 2);
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 2);
        assert!(valid_existing_json(&snapshot.join(&sources[0].output_path)).is_some());
        assert!(valid_existing_json(&snapshot.join(&sources[1].output_path)).is_some());
    }

    #[tokio::test]
    async fn retry_after_is_honored_before_success() {
        let output = temporary_output("retry-after");
        let date = test_date();
        let sources = static_sources(DataScope::TaipeiNewTaipei);
        let skip = sources
            .iter()
            .take(1)
            .map(|source| source.key.clone())
            .collect::<BTreeSet<_>>();
        seed_sources(&output, date, &skip, br#"{"cached":true}"#);
        let (client, state) = mock_client(MockMode::RetryAfterThenSuccess).await;

        let (result, sleeper, progress) = run_fetch(&output, &client, fast_timing()).await;
        let report = result.unwrap();

        assert_eq!(report.reused_count, 19);
        assert_eq!(report.downloaded_count, 1);
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 2);
        assert_eq!(sleeper.waits, vec![Duration::from_secs(7)]);
        assert!(progress.iter().any(|event| {
            matches!(
                event,
                FetchProgress::Retry {
                    source_key,
                    duration
                } if source_key == &sources[0].key && *duration == Duration::from_secs(7)
            )
        }));
    }

    #[tokio::test]
    async fn repeated_rate_limits_fail_after_bounded_retries() {
        let output = temporary_output("retry-failure");
        let date = test_date();
        let sources = static_sources(DataScope::TaipeiNewTaipei);
        let skip = sources
            .iter()
            .take(1)
            .map(|source| source.key.clone())
            .collect::<BTreeSet<_>>();
        seed_sources(&output, date, &skip, br#"{"cached":true}"#);
        let (client, state) = mock_client(MockMode::AlwaysRateLimited).await;

        let (result, sleeper, _) = run_fetch(&output, &client, fast_timing()).await;
        let error = result.unwrap_err();

        assert!(matches!(
            error,
            DataError::Fetch {
                source_name,
                error: TdxClientError::RateLimited
            } if source_name == sources[0].key
        ));
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 5);
        assert_eq!(
            sleeper.waits,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(3),
                Duration::from_secs(4),
            ]
        );
        assert!(
            !output
                .join("raw")
                .join(date.to_string())
                .join("manifest.json")
                .exists()
        );
        assert!(
            valid_existing_json(
                &output
                    .join("raw")
                    .join(date.to_string())
                    .join(&sources[1].output_path)
            )
            .is_some()
        );
    }

    #[tokio::test]
    async fn completed_rerun_makes_zero_data_requests() {
        let output = temporary_output("idempotent");
        let date = test_date();
        seed_sources(&output, date, &BTreeSet::new(), br#"{"cached":true}"#);
        let (client, state) = mock_client(MockMode::Success).await;

        let (first, _, _) = run_fetch(&output, &client, fast_timing()).await;
        let (second, _, _) = run_fetch(&output, &client, fast_timing()).await;

        assert_eq!(first.unwrap().downloaded_count, 0);
        assert_eq!(second.unwrap().reused_count, 20);
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 0);
        assert_eq!(state.token_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn manifest_contains_reused_and_downloaded_sources_with_hashes() {
        let output = temporary_output("manifest");
        let date = test_date();
        let sources = static_sources(DataScope::TaipeiNewTaipei);
        let skip = sources
            .iter()
            .take(1)
            .map(|source| source.key.clone())
            .collect::<BTreeSet<_>>();
        seed_sources(&output, date, &skip, br#"{"cached":true}"#);
        let (client, state) = mock_client(MockMode::Success).await;

        let (result, _, _) = run_fetch(&output, &client, fast_timing()).await;
        let report = result.unwrap();
        let manifest_path = output
            .join("raw")
            .join(date.to_string())
            .join("manifest.json");
        let manifest: RawSnapshotManifest =
            serde_json::from_str(&fs::read_to_string(manifest_path).unwrap()).unwrap();

        assert_eq!(report.reused_count, 19);
        assert_eq!(report.downloaded_count, 1);
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 1);
        let downloaded_body = valid_existing_json(
            &output
                .join("raw")
                .join(date.to_string())
                .join(&sources[0].output_path),
        )
        .unwrap();
        let downloaded_value: Value = serde_json::from_slice(&downloaded_body).unwrap();
        assert_eq!(downloaded_value["query"], "%24format=JSON");
        for source in sources {
            let body = fs::read(
                output
                    .join("raw")
                    .join(date.to_string())
                    .join(&source.output_path),
            )
            .unwrap();
            let source_manifest = manifest.sources.get(&source.key).unwrap();
            assert_eq!(source_manifest.endpoint, source.endpoint);
            assert_eq!(source_manifest.path, source.output_path.to_string_lossy());
            assert_eq!(source_manifest.sha256, sha256_hex(&body));
            assert!(!source_manifest.retrieved_at.is_empty());
        }
    }

    #[tokio::test]
    async fn obsolete_manifest_source_identity_forces_a_refetch() {
        let output = temporary_output("obsolete-source");
        let date = test_date();
        let sources = static_sources(DataScope::TaipeiNewTaipei);
        seed_sources(&output, date, &BTreeSet::new(), br#"{"cached":true}"#);
        let old_timetable_key = "tdx_tra_general_train_timetable";
        let old_manifest = RawSnapshotManifest {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            scope: DataScope::TaipeiNewTaipei.as_str().into(),
            date: date.to_string(),
            sources: sources
                .iter()
                .map(|source| {
                    (
                        source.key.clone(),
                        RawSourceMetadata {
                            endpoint: if source.key == old_timetable_key {
                                "v2/Rail/TRA/GeneralTimetable?$format=JSON".into()
                            } else {
                                source.endpoint.clone()
                            },
                            path: source.output_path.to_string_lossy().into_owned(),
                            sha256: sha256_hex(br#"{"cached":true}"#),
                            retrieved_at: "2026-09-15T00:00:00Z".into(),
                        },
                    )
                })
                .collect(),
        };
        let snapshot = output.join("raw").join(date.to_string());
        write_json_atomic(&snapshot.join("manifest.json"), &old_manifest).unwrap();
        let (client, state) = mock_client(MockMode::Success).await;

        let (result, _, _) = run_fetch(&output, &client, fast_timing()).await;
        let report = result.unwrap();

        assert_eq!(report.reused_count, 19);
        assert_eq!(report.downloaded_count, 1);
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            report
                .manifest
                .sources
                .get(old_timetable_key)
                .unwrap()
                .endpoint,
            "v3/Rail/TRA/GeneralTrainTimetable?$format=JSON"
        );
    }

    #[tokio::test]
    async fn permanent_bad_request_is_not_retried() {
        let output = temporary_output("permanent-400");
        let (client, state) = mock_client(MockMode::PermanentBadRequest).await;

        let (result, sleeper, _) = run_fetch(&output, &client, fast_timing()).await;

        assert!(matches!(result, Err(DataError::Fetch { .. })));
        assert_eq!(state.data_calls.load(Ordering::Relaxed), 1);
        assert!(sleeper.waits.is_empty());
    }
}
