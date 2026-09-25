//! Read the GTFS archive tables into temporary records for normalization.

use std::{collections::HashMap, path::Path};

use crate::data::DataError;

use super::{
    diagnostics::GtfsBuildDiagnostics,
    reader::{open_archive, validate_required_files},
    routes::{Route, load_routes},
    schedule::{
        CompactStopTime, FrequencyRow, GtfsCalendar, TripMeta, load_calendars, load_frequencies,
        load_stop_times, load_trips,
    },
    shapes::{ShapePoint, load_shapes},
    stops::load_stops,
};

pub(super) struct RawGtfsData {
    pub(super) stops: Vec<crate::domain::transit::TransitStop>,
    pub(super) routes: Vec<Route>,
    pub(super) calendars: HashMap<String, GtfsCalendar>,
    pub(super) trips: Vec<TripMeta>,
    pub(super) shapes: HashMap<String, Vec<ShapePoint>>,
    pub(super) frequencies: HashMap<u32, Vec<FrequencyRow>>,
    pub(super) stop_times: Vec<CompactStopTime>,
    pub(super) diagnostics: GtfsBuildDiagnostics,
}

pub(super) fn read(input: &Path) -> Result<RawGtfsData, DataError> {
    let mut archive = open_archive(input)?;
    validate_required_files(&archive, input)?;

    let mut diagnostics = GtfsBuildDiagnostics::default();
    let (stops, stop_indices) = load_stops(&mut archive, input)?;
    let routes = load_routes(&mut archive, input, &mut diagnostics)?;
    let calendars = load_calendars(&mut archive, input)?;
    let (trips, trip_indices) = load_trips(&mut archive, input, &routes)?;
    diagnostics.trips_loaded = trips.len();
    let shapes = load_shapes(&mut archive, input, &trips, &mut diagnostics)?;
    let frequencies = load_frequencies(&mut archive, input, &trip_indices)?;
    diagnostics.frequencies_loaded = frequencies.values().map(Vec::len).sum();
    let stop_times = load_stop_times(&mut archive, input, &trip_indices, &stop_indices)?;

    drop(archive);
    Ok(RawGtfsData {
        stops,
        routes,
        calendars,
        trips,
        shapes,
        frequencies,
        stop_times,
        diagnostics,
    })
}
