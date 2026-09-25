//! Parsing for stops.txt.

use std::{collections::HashMap, fs::File, path::Path};

use zip::ZipArchive;

use crate::{
    data::DataError,
    domain::{ids::BoardingPointId, location::RequestCoordinate, transit::TransitStop},
};

use super::{
    identity::tdx_stop_id,
    reader::{column, csv_error, csv_reader, required},
};

pub(super) fn load_stops(
    archive: &mut ZipArchive<File>,
    path: &Path,
) -> Result<(Vec<TransitStop>, HashMap<String, u32>), DataError> {
    let mut reader = csv_reader(archive, path, "stops.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "stops.txt", error))?
        .clone();
    let id = column(&headers, "stop_id", path, "stops.txt")?;
    let name = column(&headers, "stop_name", path, "stops.txt")?;
    let lat = column(&headers, "stop_lat", path, "stops.txt")?;
    let lon = column(&headers, "stop_lon", path, "stops.txt")?;
    let location_type = headers.iter().position(|field| field == "location_type");
    let mut stops = Vec::new();
    let mut indices = HashMap::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "stops.txt", error))?;
        if location_type
            .and_then(|index| row.get(index))
            .is_some_and(|value| !value.is_empty() && value != "0")
        {
            continue;
        }
        let raw_id = required(&row, id).trim();
        let stop_name = required(&row, name).trim();
        let Some(latitude) = required(&row, lat).parse::<f64>().ok() else {
            continue;
        };
        let Some(longitude) = required(&row, lon).parse::<f64>().ok() else {
            continue;
        };
        let coordinate = RequestCoordinate {
            latitude,
            longitude,
        };
        if !coordinate.validate() {
            continue;
        }
        if raw_id.is_empty() {
            continue;
        }
        let canonical_id = tdx_stop_id(raw_id);
        let stop_index = stops.len() as u32;
        if indices.contains_key(raw_id) {
            continue;
        }
        indices.insert(raw_id.to_owned(), stop_index);
        stops.push(TransitStop::new(
            BoardingPointId::new(canonical_id),
            if stop_name.is_empty() {
                raw_id
            } else {
                stop_name
            },
            coordinate,
        ));
    }
    if stops.is_empty() {
        return Err(DataError::EmptyNetwork);
    }
    Ok((stops, indices))
}
