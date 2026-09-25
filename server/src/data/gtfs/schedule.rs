//! Parsing for trips, stop_times, calendars, calendar_dates, and frequencies.

use std::{collections::HashMap, fs::File, path::Path};

use time::{Date, Weekday};
use zip::ZipArchive;

use crate::data::DataError;

use super::{
    identity::direction_from_route_id,
    reader::{
        column, csv_error, csv_reader, optional, parse_gtfs_date, parse_gtfs_time, required,
        weekday_for_index,
    },
    routes::Route,
};

#[derive(Clone, Debug, Default)]
pub(super) struct GtfsCalendar {
    pub(super) weekdays: Vec<Weekday>,
    pub(super) start_date: Option<Date>,
    pub(super) end_date: Option<Date>,
    pub(super) added: Vec<Date>,
    pub(super) removed: Vec<Date>,
}

#[derive(Clone, Debug)]
pub(super) struct TripMeta {
    pub(super) id: String,
    pub(super) gtfs_service_id: String,
    pub(super) route_index: usize,
    pub(super) direction: Option<u8>,
    pub(super) headsign: String,
    pub(super) shape_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct FrequencyRow {
    pub(super) start_seconds: u32,
    pub(super) end_seconds: u32,
    pub(super) headway_seconds: u32,
    pub(super) exact_times: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CompactStopTime {
    pub(super) trip_index: u32,
    pub(super) stop_index: u32,
    pub(super) sequence: u32,
    pub(super) arrival_seconds: Option<u32>,
    pub(super) departure_seconds: Option<u32>,
    pub(super) shape_distance: Option<f64>,
}

pub(super) fn load_calendars(
    archive: &mut ZipArchive<File>,
    path: &Path,
) -> Result<HashMap<String, GtfsCalendar>, DataError> {
    let mut calendars = HashMap::new();
    if archive.file_names().any(|name| name == "calendar.txt") {
        let mut reader = csv_reader(archive, path, "calendar.txt")?;
        let headers = reader
            .headers()
            .map_err(|error| csv_error(path, "calendar.txt", error))?
            .clone();
        let id = column(&headers, "service_id", path, "calendar.txt")?;
        let start = column(&headers, "start_date", path, "calendar.txt")?;
        let end = column(&headers, "end_date", path, "calendar.txt")?;
        let weekday_indices = [
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
            "sunday",
        ]
        .map(|name| column(&headers, name, path, "calendar.txt"))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        for row in reader.records() {
            let row = row.map_err(|error| csv_error(path, "calendar.txt", error))?;
            let service_id = required(&row, id).to_owned();
            let weekdays = weekday_indices
                .iter()
                .enumerate()
                .filter_map(|(index, column)| {
                    (row.get(*column) == Some("1")).then_some(weekday_for_index(index))
                })
                .collect();
            let start_date =
                parse_gtfs_date(required(&row, start)).ok_or_else(|| DataError::MalformedGtfs {
                    path: path.into(),
                    kind: "calendar.txt",
                    detail: format!("invalid start_date for service {service_id}"),
                })?;
            let end_date =
                parse_gtfs_date(required(&row, end)).ok_or_else(|| DataError::MalformedGtfs {
                    path: path.into(),
                    kind: "calendar.txt",
                    detail: format!("invalid end_date for service {service_id}"),
                })?;
            if start_date > end_date {
                return Err(DataError::MalformedGtfs {
                    path: path.into(),
                    kind: "calendar.txt",
                    detail: format!("start_date is after end_date for service {service_id}"),
                });
            }
            let calendar = GtfsCalendar {
                weekdays,
                start_date: Some(start_date),
                end_date: Some(end_date),
                ..GtfsCalendar::default()
            };
            calendars.insert(service_id, calendar);
        }
    }
    if archive
        .file_names()
        .any(|name| name == "calendar_dates.txt")
    {
        let mut reader = csv_reader(archive, path, "calendar_dates.txt")?;
        let headers = reader
            .headers()
            .map_err(|error| csv_error(path, "calendar_dates.txt", error))?
            .clone();
        let id = column(&headers, "service_id", path, "calendar_dates.txt")?;
        let date = column(&headers, "date", path, "calendar_dates.txt")?;
        let kind = column(&headers, "exception_type", path, "calendar_dates.txt")?;
        for row in reader.records() {
            let row = row.map_err(|error| csv_error(path, "calendar_dates.txt", error))?;
            let service_id = required(&row, id);
            let date =
                parse_gtfs_date(required(&row, date)).ok_or_else(|| DataError::MalformedGtfs {
                    path: path.into(),
                    kind: "calendar_dates.txt",
                    detail: format!("invalid date for service {service_id}"),
                })?;
            let calendar = calendars.entry(service_id.to_owned()).or_default();
            match required(&row, kind) {
                "1" => calendar.added.push(date),
                "2" => calendar.removed.push(date),
                _ => {}
            }
        }
    }
    for calendar in calendars.values_mut() {
        calendar.added.sort_unstable();
        calendar.added.dedup();
        calendar.removed.sort_unstable();
        calendar.removed.dedup();
    }
    Ok(calendars)
}

pub(super) fn load_trips(
    archive: &mut ZipArchive<File>,
    path: &Path,
    routes: &[Route],
) -> Result<(Vec<TripMeta>, HashMap<String, u32>), DataError> {
    let route_indices = routes
        .iter()
        .enumerate()
        .map(|(index, route)| (route.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut reader = csv_reader(archive, path, "trips.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "trips.txt", error))?
        .clone();
    let route = column(&headers, "route_id", path, "trips.txt")?;
    let service = column(&headers, "service_id", path, "trips.txt")?;
    let trip = column(&headers, "trip_id", path, "trips.txt")?;
    let direction = headers.iter().position(|field| field == "direction_id");
    let headsign = headers.iter().position(|field| field == "trip_headsign");
    let shape = headers.iter().position(|field| field == "shape_id");
    let mut trips = Vec::new();
    let mut indices = HashMap::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "trips.txt", error))?;
        let Some(route_index) = route_indices.get(required(&row, route)).copied() else {
            continue;
        };
        let id = required(&row, trip).trim();
        if id.is_empty() || indices.contains_key(id) {
            continue;
        }
        let trip_index = trips.len() as u32;
        indices.insert(id.to_owned(), trip_index);
        let direction = optional(&row, direction)
            .parse::<u8>()
            .ok()
            .or_else(|| direction_from_route_id(&routes[route_index].id));
        let shape_id = optional(&row, shape);
        trips.push(TripMeta {
            id: id.to_owned(),
            gtfs_service_id: required(&row, service).to_owned(),
            route_index,
            direction,
            headsign: optional(&row, headsign).to_owned(),
            shape_id: (!shape_id.is_empty()).then(|| shape_id.to_owned()),
        });
    }
    Ok((trips, indices))
}

pub(super) fn load_frequencies(
    archive: &mut ZipArchive<File>,
    path: &Path,
    trip_indices: &HashMap<String, u32>,
) -> Result<HashMap<u32, Vec<FrequencyRow>>, DataError> {
    if !archive.file_names().any(|name| name == "frequencies.txt") {
        return Ok(HashMap::new());
    }
    let mut reader = csv_reader(archive, path, "frequencies.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "frequencies.txt", error))?
        .clone();
    let trip = column(&headers, "trip_id", path, "frequencies.txt")?;
    let start = column(&headers, "start_time", path, "frequencies.txt")?;
    let end = column(&headers, "end_time", path, "frequencies.txt")?;
    let headway = column(&headers, "headway_secs", path, "frequencies.txt")?;
    let exact = headers.iter().position(|field| field == "exact_times");
    let mut frequencies = HashMap::<u32, Vec<FrequencyRow>>::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "frequencies.txt", error))?;
        let Some(trip_index) = trip_indices.get(required(&row, trip)).copied() else {
            continue;
        };
        let Some(start_seconds) = parse_gtfs_time(required(&row, start)) else {
            continue;
        };
        let Some(end_seconds) = parse_gtfs_time(required(&row, end)) else {
            continue;
        };
        let Some(headway_seconds) = required(&row, headway)
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
        else {
            continue;
        };
        if start_seconds > end_seconds {
            continue;
        }
        frequencies
            .entry(trip_index)
            .or_default()
            .push(FrequencyRow {
                start_seconds,
                end_seconds,
                headway_seconds,
                exact_times: optional(&row, exact) == "1",
            });
    }
    Ok(frequencies)
}

pub(super) fn load_stop_times(
    archive: &mut ZipArchive<File>,
    path: &Path,
    trip_indices: &HashMap<String, u32>,
    stop_indices: &HashMap<String, u32>,
) -> Result<Vec<CompactStopTime>, DataError> {
    let mut reader = csv_reader(archive, path, "stop_times.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "stop_times.txt", error))?
        .clone();
    let trip = column(&headers, "trip_id", path, "stop_times.txt")?;
    let stop = column(&headers, "stop_id", path, "stop_times.txt")?;
    let sequence = column(&headers, "stop_sequence", path, "stop_times.txt")?;
    let arrival = column(&headers, "arrival_time", path, "stop_times.txt")?;
    let departure = column(&headers, "departure_time", path, "stop_times.txt")?;
    let shape_distance = headers
        .iter()
        .position(|field| field == "shape_dist_traveled");
    let mut stop_times = Vec::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "stop_times.txt", error))?;
        let Some(trip_index) = trip_indices.get(required(&row, trip)).copied() else {
            continue;
        };
        let Some(stop_index) = stop_indices.get(required(&row, stop)).copied() else {
            continue;
        };
        let Some(sequence) = required(&row, sequence).parse::<u32>().ok() else {
            continue;
        };
        stop_times.push(CompactStopTime {
            trip_index,
            stop_index,
            sequence,
            arrival_seconds: parse_gtfs_time(required(&row, arrival)),
            departure_seconds: parse_gtfs_time(required(&row, departure)),
            shape_distance: optional(&row, shape_distance).parse::<f64>().ok(),
        });
    }
    stop_times.sort_unstable_by(|left, right| {
        left.trip_index
            .cmp(&right.trip_index)
            .then(left.sequence.cmp(&right.sequence))
    });
    Ok(stop_times)
}

pub(super) fn calendar_is_usable(calendar: &GtfsCalendar) -> bool {
    !calendar.weekdays.is_empty() || !calendar.added.is_empty()
}
