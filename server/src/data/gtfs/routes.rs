//! Parsing for routes.txt and GTFS route type mapping.

use std::{fs::File, path::Path};

use zip::ZipArchive;

use crate::{data::DataError, domain::transit::TransitMode};

use super::{
    diagnostics::GtfsBuildDiagnostics,
    reader::{column, csv_error, csv_reader, optional, required},
};

#[derive(Clone, Debug)]
pub(super) struct Route {
    pub(super) id: String,
    pub(super) agency_id: String,
    pub(super) mode: TransitMode,
    pub(super) label: String,
    pub(super) long_name: String,
}

pub(super) fn load_routes(
    archive: &mut ZipArchive<File>,
    path: &Path,
    diagnostics: &mut GtfsBuildDiagnostics,
) -> Result<Vec<Route>, DataError> {
    let mut reader = csv_reader(archive, path, "routes.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "routes.txt", error))?
        .clone();
    let route_id = column(&headers, "route_id", path, "routes.txt")?;
    let route_type = column(&headers, "route_type", path, "routes.txt")?;
    let short_name = headers.iter().position(|field| field == "route_short_name");
    let long_name = headers.iter().position(|field| field == "route_long_name");
    let agency = headers.iter().position(|field| field == "agency_id");
    let mut routes = Vec::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "routes.txt", error))?;
        let id = required(&row, route_id).trim();
        if id.is_empty() {
            continue;
        }
        let Some(mode) = parse_route_mode(required(&row, route_type), id) else {
            continue;
        };
        let label = optional(&row, short_name).trim();
        let long_name = optional(&row, long_name).trim();
        let label = if label.is_empty() { id } else { label };
        let long_name = if long_name.is_empty() {
            label
        } else {
            long_name
        };
        routes.push(Route {
            id: id.to_owned(),
            agency_id: optional(&row, agency).to_owned(),
            mode,
            label: label.to_owned(),
            long_name: long_name.to_owned(),
        });
        *diagnostics
            .routes_loaded_by_mode
            .entry(mode_name(mode).into())
            .or_default() += 1;
    }
    Ok(routes)
}

pub(super) fn parse_route_mode(route_type: &str, route_id: &str) -> Option<TransitMode> {
    match route_type {
        "1" => Some(TransitMode::Metro),
        "2" if route_id.starts_with("THSR") => Some(TransitMode::HighSpeedRail),
        "2" => Some(TransitMode::Rail),
        "3" => Some(TransitMode::Bus),
        _ => None,
    }
}

pub(super) fn mode_name(mode: TransitMode) -> &'static str {
    match mode {
        TransitMode::Bus => "bus",
        TransitMode::Metro => "metro",
        TransitMode::Rail => "rail",
        TransitMode::HighSpeedRail => "high_speed_rail",
    }
}
