use std::{collections::HashMap, fs::File, io::Read, path::Path, time::Instant};

use csv::StringRecord;
use gtfs_structures::{
    Calendar, CalendarDate, RawFrequency, RawGtfs, RawStopTime, RawTrip, Route, Shape,
    SourceFormat, Stop,
};
use serde::de::DeserializeOwned;
use zip::ZipArchive;

use crate::data::DataError;

const RECOGNIZED_TABLES: [&str; 9] = [
    "calendar.txt",
    "calendar_dates.txt",
    "frequencies.txt",
    "routes.txt",
    "shapes.txt",
    "stop_times.txt",
    "stops.txt",
    "transfers.txt",
    "trips.txt",
];

pub(super) fn read(path: &Path) -> Result<RawGtfs, DataError> {
    let started_at = Instant::now();
    let file = File::open(path).map_err(|error| DataError::Io(path.into(), error))?;
    let mut archive = ZipArchive::new(file).map_err(|error| invalid_archive(path, error))?;
    let mut indices = HashMap::new();
    let mut files = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| invalid_archive(path, error))?;
        let name = entry.name().to_owned();
        if let Some(file_name) = Path::new(&name).file_name().and_then(|name| name.to_str()) {
            if RECOGNIZED_TABLES.contains(&file_name) && indices.contains_key(file_name) {
                return Err(invalid_archive(
                    path,
                    format!("duplicate GTFS table {file_name}"),
                ));
            }
            indices.entry(file_name.to_owned()).or_insert(index);
        }
        files.push(name);
    }

    let calendar = read_optional::<Calendar>(&mut archive, &indices, path, "calendar.txt")?;
    let calendar_dates =
        read_optional::<CalendarDate>(&mut archive, &indices, path, "calendar_dates.txt")?;
    let routes = read_required::<Route>(&mut archive, &indices, path, "routes.txt")?;
    let stops = read_required::<Stop>(&mut archive, &indices, path, "stops.txt")?;
    let trips = read_required_with::<RawTrip>(
        &mut archive,
        &indices,
        path,
        "trips.txt",
        clear_unused_direction,
    )?;
    let stop_times = read_required_with::<RawStopTime>(
        &mut archive,
        &indices,
        path,
        "stop_times.txt",
        normalize_stop_times,
    )?;
    let frequencies =
        read_optional::<RawFrequency>(&mut archive, &indices, path, "frequencies.txt")?;
    let shapes = read_optional::<Shape>(&mut archive, &indices, path, "shapes.txt")?;

    Ok(RawGtfs {
        read_duration: started_at.elapsed(),
        calendar: calendar.map(Ok),
        calendar_dates: calendar_dates.map(Ok),
        stops: Ok(stops),
        routes: Ok(routes),
        trips: Ok(trips),
        agencies: Ok(Vec::new()),
        shapes: shapes.map(Ok),
        fare_attributes: None,
        fare_rules: None,
        fare_products: None,
        fare_media: None,
        rider_categories: None,
        frequencies: frequencies.map(Ok),
        transfers: None,
        pathways: None,
        feed_info: None,
        stop_times: Ok(stop_times),
        files,
        source_format: SourceFormat::Zip,
        sha256: None,
        translations: None,
        ticketing_deep_links: None,
        ticketing_identifiers: None,
        attributions: None,
    })
}

fn read_required<T: DeserializeOwned>(
    archive: &mut ZipArchive<File>,
    indices: &HashMap<String, usize>,
    path: &Path,
    name: &str,
) -> Result<Vec<T>, DataError> {
    read_table(archive, indices, path, name, |_, row| row.clone())?
        .ok_or_else(|| invalid_archive(path, format!("required GTFS table {name} is missing")))
}

fn read_required_with<T: DeserializeOwned>(
    archive: &mut ZipArchive<File>,
    indices: &HashMap<String, usize>,
    path: &Path,
    name: &str,
    normalize: fn(&StringRecord, &StringRecord) -> StringRecord,
) -> Result<Vec<T>, DataError> {
    read_table(archive, indices, path, name, normalize)?
        .ok_or_else(|| invalid_archive(path, format!("required GTFS table {name} is missing")))
}

fn read_optional<T: DeserializeOwned>(
    archive: &mut ZipArchive<File>,
    indices: &HashMap<String, usize>,
    path: &Path,
    name: &str,
) -> Result<Option<Vec<T>>, DataError> {
    read_table(archive, indices, path, name, |_, row| row.clone())
}

fn read_table<T: DeserializeOwned>(
    archive: &mut ZipArchive<File>,
    indices: &HashMap<String, usize>,
    path: &Path,
    name: &str,
    normalize: impl Fn(&StringRecord, &StringRecord) -> StringRecord,
) -> Result<Option<Vec<T>>, DataError> {
    let Some(index) = indices.get(name).copied() else {
        return Ok(None);
    };
    let mut entry = archive
        .by_index(index)
        .map_err(|error| invalid_archive(path, error))?;
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|error| DataError::Io(path.into(), error))?;
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        bytes.drain(..3);
    }
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(bytes.as_slice());
    let headers = reader
        .headers()
        .map_err(|error| invalid_archive(path, format!("{name}: {error}")))?
        .clone();
    let mut rows = Vec::new();
    for (row_index, record) in reader.records().enumerate() {
        let record = record.map_err(|error| {
            invalid_archive(path, format!("{name} row {}: {error}", row_index + 2))
        })?;
        let record = normalize(&headers, &record);
        rows.push(record.deserialize(Some(&headers)).map_err(|error| {
            invalid_archive(path, format!("{name} row {}: {error}", row_index + 2))
        })?);
    }
    Ok(Some(rows))
}

fn clear_unused_direction(headers: &StringRecord, row: &StringRecord) -> StringRecord {
    let Some(index) = headers.iter().position(|name| name == "direction_id") else {
        return row.clone();
    };
    if matches!(row.get(index).unwrap_or_default(), "" | "0" | "1") {
        return row.clone();
    }
    replace_field(row, index, "")
}

fn normalize_stop_times(headers: &StringRecord, row: &StringRecord) -> StringRecord {
    let replacements = ["arrival_time", "departure_time"]
        .into_iter()
        .filter_map(|name| {
            let index = headers.iter().position(|header| header == name)?;
            let value = row.get(index).unwrap_or_default();
            normalize_minute_time(value).map(|value| (index, value))
        })
        .collect::<Vec<_>>();
    if replacements.is_empty() {
        return row.clone();
    }
    let mut normalized = row.clone();
    for (index, value) in replacements {
        replace_field_in_place(&mut normalized, index, &value);
    }
    normalized
}

fn normalize_minute_time(value: &str) -> Option<String> {
    let (hours, minutes) = value.split_once(':')?;
    if minutes.contains(':')
        || hours.is_empty()
        || hours.len() > 3
        || !hours.bytes().all(|byte| byte.is_ascii_digit())
        || minutes.len() != 2
        || !minutes.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{value}:00"))
}

fn replace_field(row: &StringRecord, index: usize, value: &str) -> StringRecord {
    let mut normalized = StringRecord::new();
    for (field_index, field) in row.iter().enumerate() {
        normalized.push_field(if field_index == index { value } else { field });
    }
    normalized
}

fn replace_field_in_place(row: &mut StringRecord, index: usize, value: &str) {
    let mut normalized = StringRecord::new();
    for (field_index, field) in row.iter().enumerate() {
        normalized.push_field(if field_index == index { value } else { field });
    }
    *row = normalized;
}

fn invalid_archive(path: &Path, detail: impl std::fmt::Display) -> DataError {
    DataError::InvalidGtfsArchive {
        path: path.into(),
        detail: detail.to_string(),
    }
}
