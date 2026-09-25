//! Shared ZIP, CSV, and GTFS scalar parsing helpers.

use std::{collections::HashSet, fs::File, io::Read, path::Path};

use csv::{Reader, StringRecord};
use sha2::{Digest, Sha256};
use time::{Date, Month, Weekday};
use zip::{ZipArchive, read::ZipFile};

use crate::data::DataError;

pub(super) fn parse_gtfs_time(value: &str) -> Option<u32> {
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<u32>().ok()?;
    let minutes = parts.next()?.parse::<u32>().ok()?;
    let seconds = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || minutes > 59 || seconds > 59 {
        return None;
    }
    hours
        .checked_mul(3600)?
        .checked_add(minutes * 60)?
        .checked_add(seconds)
}

pub(super) fn parse_gtfs_date(value: &str) -> Option<Date> {
    if value.len() != 8 {
        return None;
    }
    let year = value.get(..4)?.parse::<i32>().ok()?;
    let month = Month::try_from(value.get(4..6)?.parse::<u8>().ok()?).ok()?;
    let day = value.get(6..8)?.parse::<u8>().ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

pub(super) fn weekday_for_index(index: usize) -> Weekday {
    match index {
        0 => Weekday::Monday,
        1 => Weekday::Tuesday,
        2 => Weekday::Wednesday,
        3 => Weekday::Thursday,
        4 => Weekday::Friday,
        5 => Weekday::Saturday,
        _ => Weekday::Sunday,
    }
}

pub(super) fn hash_file(path: &Path) -> Result<String, DataError> {
    let mut file = File::open(path).map_err(|error| DataError::Io(path.into(), error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| DataError::Io(path.into(), error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn open_archive(path: &Path) -> Result<ZipArchive<File>, DataError> {
    let file = File::open(path).map_err(|error| DataError::Io(path.into(), error))?;
    ZipArchive::new(file).map_err(|error| invalid_archive(path, error))
}

pub(super) fn validate_required_files(
    archive: &ZipArchive<File>,
    path: &Path,
) -> Result<(), DataError> {
    let names = archive
        .file_names()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for required in ["stops.txt", "routes.txt", "trips.txt", "stop_times.txt"] {
        if !names.contains(required) {
            return Err(invalid_archive(
                path,
                format!("missing required {required}"),
            ));
        }
    }
    Ok(())
}

pub(super) fn csv_reader<'a>(
    archive: &'a mut ZipArchive<File>,
    path: &Path,
    name: &str,
) -> Result<Reader<ZipFile<'a>>, DataError> {
    let file = archive
        .by_name(name)
        .map_err(|error| invalid_archive(path, format!("could not read {name}: {error}")))?;
    Ok(csv::ReaderBuilder::new().flexible(true).from_reader(file))
}

pub(super) fn column(
    headers: &StringRecord,
    name: &str,
    path: &Path,
    file: &'static str,
) -> Result<usize, DataError> {
    headers
        .iter()
        .position(|header| header.trim_start_matches('\u{feff}') == name)
        .ok_or_else(|| DataError::MalformedGtfs {
            path: path.into(),
            kind: file,
            detail: format!("missing required column {name}"),
        })
}

pub(super) fn required(row: &StringRecord, index: usize) -> &str {
    row.get(index).unwrap_or("")
}

pub(super) fn optional(row: &StringRecord, index: Option<usize>) -> &str {
    index.and_then(|index| row.get(index)).unwrap_or("")
}

pub(super) fn invalid_archive(path: &Path, error: impl std::fmt::Display) -> DataError {
    DataError::InvalidGtfsArchive {
        path: path.into(),
        detail: error.to_string(),
    }
}

pub(super) fn csv_error(path: &Path, name: &'static str, error: csv::Error) -> DataError {
    DataError::MalformedGtfs {
        path: path.into(),
        kind: name,
        detail: error.to_string(),
    }
}
