//! Offline TDX snapshot, normalization, and artifact tooling.
//!
//! Provider payloads stop at this module. The routing and domain modules only
//! receive the validated provider-neutral `TransitNetwork` artifact.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Date, Weekday, format_description::well_known::Rfc3339};

use crate::{
    config::{TdxClient, TdxClientError, TdxCredentials},
    domain::{
        ids::{BoardingPointId, ScheduledRunId, TransitServiceId},
        transit::{
            BikeShareStation, ScheduledStopTime, ScheduledTrip, TransferEdge, TransitMode,
            TransitNetwork, TransitService, TransitStop, default_service_day_boundary,
        },
    },
    providers::tdx::{
        bike_share_station_id, boarding_point_id, transit_service_id,
        transit_service_id_for_direction,
    },
    routing::transfers::{TransferShortcut, TransferShortcutIndex},
};

pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;
const FETCH_CONCURRENCY: usize = 4;
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferPreprocessingConfig {
    pub max_walk_access_metres: u32,
    pub max_walk_egress_metres: u32,
    pub max_bike_distance_metres: u32,
    pub max_alternatives_per_pair: usize,
}

impl Default for TransferPreprocessingConfig {
    fn default() -> Self {
        Self {
            max_walk_access_metres: 600,
            max_walk_egress_metres: 600,
            max_bike_distance_metres: 8_000,
            max_alternatives_per_pair: 4,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactCounts {
    pub stops: usize,
    pub services: usize,
    pub scheduled_trips: usize,
    pub bike_stations: usize,
    pub transfer_shortcuts: usize,
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
    pub transfer_config: TransferPreprocessingConfig,
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

#[derive(Clone, Debug)]
pub struct BuildOutput {
    pub network: TransitNetwork,
    pub manifest: ArtifactManifest,
    pub transfer_index: TransferShortcutIndex,
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
    #[error("transfer shortcut artifact is invalid: {0}")]
    InvalidTransferArtifact(String),
}

#[derive(Clone, Debug)]
struct StaticSource {
    key: String,
    path: String,
    endpoint: String,
    output_path: PathBuf,
}

#[derive(Debug)]
struct RawDocument {
    relative_path: PathBuf,
    bytes: Vec<u8>,
    value: Value,
}

type MetroPattern = (String, TransitMode, Vec<(u32, BoardingPointId)>);

#[derive(Default)]
struct Normalizer {
    stops: BTreeMap<String, TransitStop>,
    services: BTreeMap<String, TransitService>,
    trips: BTreeMap<String, ScheduledTrip>,
    bike_stations: BTreeMap<String, BikeShareStation>,
    transfer_edges: BTreeSet<(String, String, u32)>,
    station_aliases: BTreeMap<String, BoardingPointId>,
    metro_patterns: BTreeMap<String, MetroPattern>,
}

impl Normalizer {
    fn normalize_document(&mut self, document: &RawDocument) -> Result<(), DataError> {
        let path_text = document
            .relative_path
            .to_string_lossy()
            .to_ascii_lowercase();
        let records = records(&document.value);
        for (index, record) in records.iter().enumerate() {
            if path_text.contains("bike") {
                self.normalize_bike(record, &document.relative_path)?;
            }
            if path_text.contains("bus") {
                self.normalize_bus(record, &document.relative_path, index)?;
            }
            if path_text.contains("metro") || path_text.contains("rail") {
                self.normalize_rail(record, &document.relative_path, index, &path_text)?;
            }
            if path_text.contains("transfer") {
                self.normalize_transfer(record, &document.relative_path)?;
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
    ) -> Result<(), DataError> {
        if let Some(stops) = record.get("Stops").and_then(Value::as_array) {
            let sub_route = string(record, "SubRouteUID").or_else(|| string(record, "ServiceUID"));
            let Some(sub_route) = sub_route else {
                return Ok(());
            };
            let direction = number(record, "Direction").and_then(|value| u8::try_from(value).ok());
            let service_id = direction
                .map(|direction| transit_service_id_for_direction(sub_route, direction))
                .unwrap_or_else(|| transit_service_id(sub_route));
            let mut ordered = stops
                .iter()
                .enumerate()
                .map(|(fallback, stop)| {
                    (
                        number(stop, "StopSequence").unwrap_or(fallback as u64),
                        stop,
                    )
                })
                .collect::<Vec<_>>();
            ordered.sort_by_key(|(sequence, _)| *sequence);
            let mut pattern = Vec::with_capacity(ordered.len());
            for (_, stop) in ordered {
                pattern.push(self.normalize_stop(stop, path, "bus")?);
            }
            self.insert_service(
                TransitService::new(
                    service_id,
                    TransitMode::Bus,
                    display_name(record, &["SubRouteName", "RouteName"])
                        .unwrap_or_else(|| sub_route.into()),
                    display_name(record, &["DestinationStopNameZh", "DestinationStopName"])
                        .unwrap_or_default(),
                    pattern,
                )
                .with_timing_source("bus_n1"),
                path,
            )?;
            self.normalize_embedded_trip(record, path, index, TransitMode::Bus)?;
            return Ok(());
        }
        if string(record, "StopUID").is_some() {
            let _ = self.normalize_stop(record, path, "bus")?;
        }
        if record.get("StopTimes").is_some()
            || record.get("stop_times").is_some()
            || record.get("Timetables").and_then(Value::as_array).is_some()
        {
            self.normalize_embedded_trip(record, path, index, TransitMode::Bus)?;
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
        if let Some(station_uid) = string(record, "StationUID") {
            let id = self.normalize_rail_station(record, station_uid, path, mode)?;
            if let Some(line_id) = string(record, "LineID").or_else(|| string(record, "RouteUID")) {
                let sequence = number(record, "StationSequence")
                    .or_else(|| number(record, "Sequence"))
                    .unwrap_or(index as u64) as u32;
                let entry = self
                    .metro_patterns
                    .entry(line_id.into())
                    .or_insert_with(|| (line_id.into(), mode, Vec::new()));
                entry.2.push((sequence, id));
            }
        }
        if record.get("Stations").and_then(Value::as_array).is_some()
            || record.get("StopTimes").is_some()
            || record.get("stop_times").is_some()
            || record.get("Timetables").is_some()
            || record.get("TrainInfo").is_some()
        {
            self.normalize_embedded_trip(record, path, index, mode)?;
        }
        Ok(())
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
                    )?;
                }
            }
            return Ok(());
        };
        if stop_times.len() < 2 {
            return Ok(());
        }
        let operator = match mode {
            TransitMode::HighSpeedRail => "thsr",
            TransitMode::Rail => "tra",
            TransitMode::Metro => "metro",
            TransitMode::Bus => "bus",
        };
        let train_info = record.get("TrainInfo").unwrap_or(record);
        let train_no = string(train_info, "TrainNo")
            .or_else(|| string(record, "TripID"))
            .or_else(|| string(record, "TripId"))
            .or_else(|| string(record, "ServiceUID"))
            .or_else(|| string(record, "SubRouteUID"))
            .unwrap_or("trip");
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
        for (fallback, stop_time) in stop_times.iter().enumerate() {
            let stop_id = self.resolve_timetable_stop(stop_time, path, mode, fallback)?;
            let arrival = time_seconds(
                string(stop_time, "ArrivalTime")
                    .or_else(|| string(stop_time, "arrival_time"))
                    .or_else(|| string(stop_time, "Time")),
            )
            .ok_or_else(|| malformed(path, "scheduled arrival time", train_no))?;
            let departure = time_seconds(
                string(stop_time, "DepartureTime")
                    .or_else(|| string(stop_time, "departure_time"))
                    .or_else(|| string(stop_time, "Time")),
            )
            .unwrap_or(arrival);
            parsed.push((stop_id, ScheduledStopTime::new(arrival, departure)));
        }
        let pattern = parsed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        self.insert_service(
            TransitService::new(
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
            }),
            path,
        )?;
        let trip_id = string(train_info, "TrainDate")
            .map(|date| format!("{operator}:{train_no}:{date}"))
            .unwrap_or_else(|| format!("{operator}:{train_no}:{index}"));
        let (active_weekdays, added_days, removed_days) = service_calendar(record);
        let trip = ScheduledTrip::weekdays(
            ScheduledRunId::new(trip_id),
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
            if let Some(coordinate) = coordinate(record) {
                let stop = TransitStop::new(
                    boarding_point_id(station_uid),
                    display_name(record, &["StationName", "StopName"])
                        .unwrap_or_else(|| station_uid.into()),
                    coordinate,
                );
                self.insert_stop(stop, path)?;
            }
            return Ok(boarding_point_id(station_uid));
        }
        let local = string(record, "StationID").or_else(|| string(record, "StopID"));
        if let Some(local) = local {
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
        let id = boarding_point_id(stop_uid);
        let Some(coordinate) =
            coordinate(record).or_else(|| self.stops.get(stop_uid).map(|stop| stop.coordinate))
        else {
            return Err(malformed(path, kind, stop_uid));
        };
        let stop = TransitStop::new(
            id.clone(),
            display_name(record, &["StopName", "StopNameZh", "StopNameEn"])
                .or_else(|| self.stops.get(stop_uid).map(|stop| stop.name.clone()))
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

    fn insert_service(&mut self, service: TransitService, path: &Path) -> Result<(), DataError> {
        let key = service.id.as_str().to_owned();
        if let Some(existing) = self.services.get(&key) {
            if existing.pattern != service.pattern {
                return Err(DataError::ConflictingIdentity {
                    path: path.into(),
                    id: key,
                    detail: "TransitService patterns disagree".into(),
                });
            }
        } else {
            self.services.insert(key, service);
        }
        Ok(())
    }

    fn insert_trip(&mut self, trip: ScheduledTrip, path: &Path) -> Result<(), DataError> {
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
            self.transfer_edges
                .insert((from.into(), to.into(), duration));
        } else if record.get("FromStopUID").is_some() || record.get("from").is_some() {
            return Err(malformed(
                path,
                "transfer edge",
                "missing endpoint or duration",
            ));
        }
        Ok(())
    }

    fn finish(mut self) -> TransitNetwork {
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
        for (line_id, (label, mode, mut pattern)) in self.metro_patterns {
            pattern.sort_by_key(|(sequence, _)| *sequence);
            pattern.dedup_by(|left, right| left.1 == right.1);
            let pattern = pattern.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
            if pattern.len() >= 2 {
                let service_id = TransitServiceId::new(format!("metro:{line_id}"));
                self.services
                    .entry(service_id.as_str().into())
                    .or_insert_with(|| {
                        TransitService::new(service_id, mode, label, "", pattern)
                            .with_timing_source("metro_schedule")
                    });
            }
        }
        TransitNetwork::new(
            default_service_day_boundary(),
            self.stops.into_values().collect(),
            self.services.into_values().collect(),
            self.trips.into_values().collect(),
            self.bike_stations.into_values().collect(),
        )
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
        )
    }
}

pub fn build_from_raw(
    input: &Path,
    output: &Path,
    scope: Option<DataScope>,
    raw_snapshot_date: Option<String>,
) -> Result<BuildOutput, DataError> {
    let documents = read_documents(input)?;
    if documents.is_empty() {
        return Err(DataError::EmptySnapshot(input.into()));
    }
    let mut normalizer = Normalizer::default();
    for document in &documents {
        normalizer.normalize_document(document)?;
    }
    let mut network = normalizer.finish().canonicalize();
    if network.stops.is_empty() {
        return Err(DataError::EmptyNetwork);
    }
    let static_revision = revision_for_network(&network, false)?;
    let schedule_revision = revision_for_network(&network, true)?;
    network = network.with_revisions(static_revision.clone(), schedule_revision.clone());
    network.validate()?;
    let transfer_index = documents
        .iter()
        .find(|document| {
            document
                .relative_path
                .file_name()
                .is_some_and(|name| name == "transfer-shortcuts.json")
        })
        .map(|document| {
            TransferShortcutIndex::from_artifact_json(
                std::str::from_utf8(&document.bytes)
                    .map_err(|error| DataError::InvalidTransferArtifact(error.to_string()))?,
            )
            .map_err(|error| DataError::InvalidTransferArtifact(error.to_string()))
        })
        .transpose()?
        .unwrap_or_else(|| {
            TransferShortcutIndex::new(
                network
                    .transfer_edges
                    .iter()
                    .filter(|edge| edge.duration_seconds > 0)
                    .map(|edge| {
                        TransferShortcut::walking(
                            edge.from.clone(),
                            edge.to.clone(),
                            edge.duration_seconds,
                            0,
                        )
                    }),
            )
        });
    transfer_index
        .validate_against(&network)
        .map_err(DataError::InvalidTransferArtifact)?;

    let sources = documents
        .iter()
        .map(|document| {
            (
                document.relative_path.to_string_lossy().into_owned(),
                sha256_hex(&document.bytes),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let raw_manifest = input.join("manifest.json");
    let raw_manifest = fs::read_to_string(&raw_manifest)
        .ok()
        .and_then(|body| serde_json::from_str::<RawSnapshotManifest>(&body).ok());
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
            transfer_shortcuts: transfer_index.len(),
        },
        revisions: ArtifactRevisions {
            static_network_revision: static_revision,
            schedule_revision,
        },
        transfer_config: TransferPreprocessingConfig::default(),
    };
    write_network_artifact(output, &network)?;
    write_transfer_artifact(output, &transfer_index)?;
    write_json_atomic(&output.join("manifest.json"), &manifest)?;
    Ok(BuildOutput {
        network,
        manifest,
        transfer_index,
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

pub fn load_transfer_artifact(path: &Path) -> Result<TransferShortcutIndex, DataError> {
    let body = fs::read_to_string(path).map_err(|error| DataError::Io(path.into(), error))?;
    TransferShortcutIndex::from_artifact_json(&body)
        .map_err(|error| DataError::InvalidTransferArtifact(error.to_string()))
}

pub fn write_transfer_artifact(
    output: &Path,
    index: &TransferShortcutIndex,
) -> Result<(), DataError> {
    let body = index
        .canonical_json()
        .map_err(|error| DataError::InvalidTransferArtifact(error.to_string()))?;
    write_text_atomic(&output.join("transfer-shortcuts.json"), &body)
}

pub async fn fetch_raw_snapshot(
    scope: DataScope,
    date: Date,
    output: &Path,
    credentials: TdxCredentials,
) -> Result<RawSnapshotManifest, DataError> {
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
    let sources = static_sources(scope, date);
    let snapshot = output.join("raw").join(date.to_string());
    fs::create_dir_all(&snapshot).map_err(|error| DataError::Io(snapshot.clone(), error))?;
    let fetched = stream::iter(sources.into_iter().map(|source| {
        let client = client.clone();
        async move {
            let result = client.get_raw(&source.path, &[]).await;
            (source, result)
        }
    }))
    .buffer_unordered(FETCH_CONCURRENCY)
    .map(|(source, result)| {
        let snapshot = snapshot.clone();
        async move {
            let response = result.map_err(|error| DataError::Fetch {
                source_name: source.key.clone(),
                error,
            })?;
            let path = snapshot.join(&source.output_path);
            write_bytes_atomic(&path, &response.body)?;
            Ok::<_, DataError>((source, response))
        }
    })
    .buffer_unordered(FETCH_CONCURRENCY)
    .try_collect::<Vec<_>>()
    .await?;
    let mut metadata = BTreeMap::new();
    for (source, response) in fetched {
        metadata.insert(
            source.key,
            RawSourceMetadata {
                endpoint: source.endpoint,
                path: source.output_path.to_string_lossy().into_owned(),
                sha256: sha256_hex(&response.body),
                retrieved_at: time::OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "unknown".into()),
            },
        );
    }
    let manifest = RawSnapshotManifest {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        scope: scope.as_str().into(),
        date: date.to_string(),
        sources: metadata,
    };
    write_json_atomic(&snapshot.join("manifest.json"), &manifest)?;
    Ok(manifest)
}

fn static_sources(scope: DataScope, date: Date) -> Vec<StaticSource> {
    let mut sources = Vec::new();
    for (city_key, city) in scope.cities() {
        for (name, endpoint) in [
            ("route", format!("v2/Bus/Route/City/{city}")),
            ("stop", format!("v2/Bus/Stop/City/{city}")),
            ("stop-of-route", format!("v2/Bus/StopOfRoute/City/{city}")),
            ("schedule", format!("v2/Bus/Schedule/City/{city}")),
        ] {
            sources.push(source(
                format!("tdx_bus_{city_key}_{name}"),
                endpoint,
                format!("bus/{city_key}/{name}.json"),
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
            "tdx_metro_station_timetable",
            "v2/Rail/Metro/StationTimeTable/TRTC".into(),
            "metro/trtc-station-timetable.json".into(),
        ),
        source(
            "tdx_tra_station",
            "v2/Rail/TRA/Station".into(),
            "tra/station.json".into(),
        ),
        source(
            "tdx_tra_general_timetable",
            "v2/Rail/TRA/GeneralTimetable".into(),
            "tra/general-timetable.json".into(),
        ),
        source(
            "tdx_thsr_station",
            "v2/Rail/THSR/Station".into(),
            "thsr/station.json".into(),
        ),
        source(
            "tdx_thsr_daily_timetable",
            format!("v2/Rail/THSR/DailyTimetable/TrainDate/{}", date),
            "thsr/daily-timetable.json".into(),
        ),
    ]);
    sources
}

fn source(key: impl Into<String>, path: String, output_path: String) -> StaticSource {
    StaticSource {
        key: key.into(),
        endpoint: path.clone(),
        path,
        output_path: output_path.into(),
    }
}

fn read_documents(input: &Path) -> Result<Vec<RawDocument>, DataError> {
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
            Ok(RawDocument {
                relative_path,
                bytes,
                value,
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

fn records(value: &Value) -> Vec<&Value> {
    if let Some(array) = value.as_array() {
        return array.iter().collect();
    }
    if let Some(array) = value.get("value").and_then(Value::as_array) {
        return array.iter().collect();
    }
    vec![value]
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
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

fn service_calendar(value: &Value) -> (Vec<Weekday>, Vec<Date>, Vec<Date>) {
    let active = value
        .get("ActiveWeekdays")
        .or_else(|| value.get("OperatingDays"))
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(parse_weekday).collect::<Vec<_>>())
        .filter(|days| !days.is_empty())
        .unwrap_or_else(|| ALL_WEEKDAYS.to_vec());
    let added = value
        .get("AddedServiceDays")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().and_then(parse_date))
                .collect()
        })
        .unwrap_or_default();
    let removed = value
        .get("RemovedServiceDays")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().and_then(parse_date))
                .collect()
        })
        .unwrap_or_default();
    (active, added, removed)
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
