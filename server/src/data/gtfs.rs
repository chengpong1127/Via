//! GTFS-only static transit normalization.
//!
//! The archive is read in place. Realtime providers remain responsible for
//! live timing and service conditions; the Bus costs here are Phase 1 ranking
//! inputs and are never written into scheduled stop times.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::Read,
    path::Path,
};

use csv::{Reader, StringRecord};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{Date, Month, Weekday};
use zip::{ZipArchive, read::ZipFile};

use crate::{
    data::{
        ARTIFACT_SCHEMA_VERSION, ArtifactCounts, ArtifactManifest, ArtifactRevisions, BuildOutput,
        DataError, write_bikeshare_artifact,
    },
    domain::{
        bikeshare::BikeShareNetwork,
        ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
        location::RequestCoordinate,
        transit::{
            DerivedBusTravelTime, ScheduledStopTime, ScheduledTrip, TransferEdge, TransitFrequency,
            TransitMode, TransitNetwork, TransitService, TransitStop, default_service_day_boundary,
        },
    },
    tdx::transit_service_id_for_direction,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GtfsBuildDiagnostics {
    pub stops_loaded: usize,
    pub routes_loaded_by_mode: BTreeMap<String, usize>,
    pub services_loaded_by_mode: BTreeMap<String, usize>,
    pub trips_loaded: usize,
    pub scheduled_trips_loaded: usize,
    pub frequencies_loaded: usize,
    pub transfer_edges_loaded: usize,
    pub shape_rows_loaded: usize,
    pub invalid_timing_trips: usize,
    pub invalid_timing_trip_examples: Vec<String>,
    pub exact_static_timing_services: usize,
    pub derived_bus_timing_services: usize,
    pub geometric_fallback_services: usize,
    pub unresolved_bus_realtime_scope_services: usize,
    pub unresolved_bus_realtime_scope_examples: Vec<String>,
    pub gtfs_stop_ids_without_tdx_mapping: usize,
    pub unmapped_stop_id_examples: Vec<String>,
    pub gtfs_service_ids_without_tdx_mapping: usize,
    pub unmapped_service_id_examples: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct GtfsBuildOutput {
    pub build: BuildOutput,
    pub diagnostics: GtfsBuildDiagnostics,
}

#[derive(Clone, Debug)]
struct Route {
    id: String,
    agency_id: String,
    mode: TransitMode,
    label: String,
    long_name: String,
}

#[derive(Clone, Debug)]
struct TripMeta {
    id: String,
    gtfs_service_id: String,
    route_index: usize,
    direction: Option<u8>,
    headsign: String,
    shape_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ServiceCalendar {
    weekdays: Vec<Weekday>,
    start_date: Option<Date>,
    end_date: Option<Date>,
    added: Vec<Date>,
    removed: Vec<Date>,
}

#[derive(Clone, Debug)]
struct FrequencyRow {
    start_seconds: u32,
    end_seconds: u32,
    headway_seconds: u32,
    exact_times: bool,
}

#[derive(Clone, Copy, Debug)]
struct CompactStopTime {
    trip_index: u32,
    stop_index: u32,
    sequence: u32,
    arrival_seconds: Option<u32>,
    departure_seconds: Option<u32>,
    shape_distance: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct ShapePoint {
    latitude: f64,
    longitude: f64,
    distance: f64,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ServicePatternKey {
    route_index: usize,
    direction: Option<u8>,
    pattern: Vec<u32>,
}

#[derive(Clone, Debug)]
struct ServiceBuilder {
    route_index: usize,
    direction: Option<u8>,
    pattern: Vec<u32>,
    headsigns: BTreeMap<String, usize>,
    scheduled_trips: Vec<ScheduledTrip>,
    frequencies: Vec<TransitFrequency>,
    bus_segment_samples: Vec<Vec<u32>>,
    has_exact_timing: bool,
}

impl ServiceBuilder {
    fn new(key: &ServicePatternKey) -> Self {
        Self {
            route_index: key.route_index,
            direction: key.direction,
            pattern: key.pattern.clone(),
            headsigns: BTreeMap::new(),
            scheduled_trips: Vec::new(),
            frequencies: Vec::new(),
            bus_segment_samples: vec![Vec::new(); key.pattern.len().saturating_sub(1)],
            has_exact_timing: false,
        }
    }

    fn add_headsign(&mut self, value: &str) {
        if !value.trim().is_empty() {
            *self.headsigns.entry(value.trim().to_owned()).or_default() += 1;
        }
    }

    fn headsign(&self, route: &Route) -> String {
        self.headsigns
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
            .map(|(value, _)| value.clone())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| route.long_name.clone())
    }
}

pub fn build_from_gtfs_archive(input: &Path, output: &Path) -> Result<GtfsBuildOutput, DataError> {
    let file = File::open(input).map_err(|error| DataError::Io(input.into(), error))?;
    let mut archive = ZipArchive::new(file).map_err(|error| invalid_archive(input, error))?;
    let names = archive
        .file_names()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for required in ["stops.txt", "routes.txt", "trips.txt", "stop_times.txt"] {
        if !names.contains(required) {
            return Err(invalid_archive(
                input,
                format!("missing required {required}"),
            ));
        }
    }

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
    let transfers = load_transfers(
        &mut archive,
        input,
        &stop_indices,
        &stops,
        names.contains("transfers.txt"),
    )?;

    let (
        mut services,
        trips_out,
        mut frequencies_out,
        derived_bus_times,
        timing_classes,
        invalid_timing_trips,
    ) = normalize_patterns(
        &stops,
        &routes,
        &trips,
        &calendars,
        &frequencies,
        &stop_times,
        &shapes,
    );
    diagnostics.invalid_timing_trips = invalid_timing_trips.0;
    diagnostics.invalid_timing_trip_examples = invalid_timing_trips.1;
    drop(archive);
    drop(stop_indices);
    drop(calendars);
    drop(trip_indices);
    drop(trips);
    drop(shapes);
    drop(frequencies);
    drop(stop_times);
    diagnostics.scheduled_trips_loaded = trips_out.len();
    diagnostics.frequencies_loaded = frequencies_out.len();
    diagnostics.transfer_edges_loaded = transfers.len();
    diagnostics.exact_static_timing_services = timing_classes.0;
    diagnostics.derived_bus_timing_services = timing_classes.1;
    diagnostics.geometric_fallback_services = timing_classes.2;
    for service in &services {
        *diagnostics
            .services_loaded_by_mode
            .entry(mode_name(service.transit_mode).to_owned())
            .or_default() += 1;
    }
    let unresolved_bus_scopes = services
        .iter()
        .filter(|service| {
            service.transit_mode == TransitMode::Bus && service.realtime_scope.is_none()
        })
        .map(|service| service.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.unresolved_bus_realtime_scope_services = unresolved_bus_scopes.len();
    diagnostics.unresolved_bus_realtime_scope_examples =
        unresolved_bus_scopes.into_iter().take(20).collect();
    let used_stop_ids = services
        .iter()
        .flat_map(|service| service.pattern.iter().cloned())
        .chain(
            transfers
                .iter()
                .flat_map(|edge| [edge.from.clone(), edge.to.clone()]),
        )
        .collect::<HashSet<_>>();
    let stops = stops
        .into_iter()
        .filter(|stop| used_stop_ids.contains(&stop.id))
        .collect::<Vec<_>>();
    diagnostics.stops_loaded = stops.len();
    let unmapped_stops = stops
        .iter()
        .filter(|stop| !is_tdx_stop_id(stop.id.as_str()))
        .map(|stop| stop.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.gtfs_stop_ids_without_tdx_mapping = unmapped_stops.len();
    diagnostics.unmapped_stop_id_examples = unmapped_stops.into_iter().take(20).collect();
    let unmapped_services = services
        .iter()
        .filter(|service| !is_tdx_service_id(service, &routes))
        .map(|service| service.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.gtfs_service_ids_without_tdx_mapping = unmapped_services.len();
    diagnostics.unmapped_service_id_examples = unmapped_services.into_iter().take(20).collect();

    let derived_bus_times = derived_bus_times;
    services.shrink_to_fit();
    frequencies_out.shrink_to_fit();
    let network = TransitNetwork::new(default_service_day_boundary(), stops, services, trips_out)
        .with_frequencies(frequencies_out)
        .with_derived_bus_travel_times(derived_bus_times)
        .with_transfer_edges(transfers)
        .canonicalize();

    let bike_share_network = BikeShareNetwork::new(Vec::new());
    let static_revision = super::revision_for_network(&network, false)?;
    let schedule_revision = super::revision_for_network(&network, true)?;
    let network = network.with_revisions(static_revision.clone(), schedule_revision.clone());
    network.validate()?;
    let bike_share_revision = super::revision_for_bike_share_network(&bike_share_network)?;
    let bike_share_network = bike_share_network.with_revision(bike_share_revision.clone());
    bike_share_network.validate()?;
    let source_hash = hash_file(input)?;
    let manifest = ArtifactManifest {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        scope: "taiwan-gtfs".into(),
        built_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into()),
        raw_snapshot_date: None,
        sources: BTreeMap::from([(
            input
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("gtfs.zip")
                .to_owned(),
            source_hash,
        )]),
        counts: ArtifactCounts {
            stops: network.stops.len(),
            services: network.services.len(),
            scheduled_trips: network.trips.len(),
            transfer_edges: network.transfer_edges.len(),
            bike_stations: bike_share_network.stations.len(),
            bike_access_links: bike_share_network.access_links.len(),
        },
        revisions: ArtifactRevisions {
            static_network_revision: static_revision,
            schedule_revision,
            bike_share_network_revision: bike_share_revision,
        },
    };
    super::write_network_artifact(output, &network)?;
    write_bikeshare_artifact(output, &bike_share_network)?;
    super::write_json_atomic(&output.join("manifest.json"), &manifest)?;
    Ok(GtfsBuildOutput {
        build: BuildOutput {
            network,
            bike_share_network,
            manifest,
            diagnostics: Default::default(),
        },
        diagnostics,
    })
}

fn load_stops(
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

fn load_routes(
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

fn load_calendars(
    archive: &mut ZipArchive<File>,
    path: &Path,
) -> Result<HashMap<String, ServiceCalendar>, DataError> {
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
            let calendar = ServiceCalendar {
                weekdays,
                start_date: Some(start_date),
                end_date: Some(end_date),
                ..ServiceCalendar::default()
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

fn load_trips(
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

fn load_shapes(
    archive: &mut ZipArchive<File>,
    path: &Path,
    trips: &[TripMeta],
    diagnostics: &mut GtfsBuildDiagnostics,
) -> Result<HashMap<String, Vec<ShapePoint>>, DataError> {
    if !archive.file_names().any(|name| name == "shapes.txt") {
        return Ok(HashMap::new());
    }
    let used = trips
        .iter()
        .filter_map(|trip| trip.shape_id.as_deref())
        .collect::<HashSet<_>>();
    let mut reader = csv_reader(archive, path, "shapes.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "shapes.txt", error))?
        .clone();
    let shape_id = column(&headers, "shape_id", path, "shapes.txt")?;
    let lat = column(&headers, "shape_pt_lat", path, "shapes.txt")?;
    let lon = column(&headers, "shape_pt_lon", path, "shapes.txt")?;
    let sequence = column(&headers, "shape_pt_sequence", path, "shapes.txt")?;
    let traveled = headers
        .iter()
        .position(|field| field == "shape_dist_traveled");
    let mut shapes = HashMap::<String, Vec<(u32, f64, f64, Option<f64>)>>::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "shapes.txt", error))?;
        let id = required(&row, shape_id);
        if !used.contains(id) {
            continue;
        }
        let Some(latitude) = required(&row, lat).parse::<f64>().ok() else {
            continue;
        };
        let Some(longitude) = required(&row, lon).parse::<f64>().ok() else {
            continue;
        };
        let Some(sequence) = required(&row, sequence).parse::<u32>().ok() else {
            continue;
        };
        let distance = optional(&row, traveled).parse::<f64>().ok();
        shapes
            .entry(id.to_owned())
            .or_default()
            .push((sequence, latitude, longitude, distance));
        diagnostics.shape_rows_loaded += 1;
    }
    Ok(shapes
        .into_iter()
        .map(|(id, mut points)| {
            points.sort_by_key(|point| point.0);
            let mut cumulative = 0.0;
            let mut previous = None;
            let points = points
                .into_iter()
                .map(|(_, latitude, longitude, explicit)| {
                    if let Some(distance) = explicit.filter(|distance| distance.is_finite()) {
                        cumulative = distance;
                    } else if let Some((previous_latitude, previous_longitude)) = previous {
                        cumulative += geographic_distance_metres(
                            previous_latitude,
                            previous_longitude,
                            latitude,
                            longitude,
                        );
                    }
                    previous = Some((latitude, longitude));
                    ShapePoint {
                        latitude,
                        longitude,
                        distance: cumulative,
                    }
                })
                .collect();
            (id, points)
        })
        .collect())
}

fn load_frequencies(
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

fn load_stop_times(
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

fn load_transfers(
    archive: &mut ZipArchive<File>,
    path: &Path,
    stop_indices: &HashMap<String, u32>,
    stops: &[TransitStop],
    present: bool,
) -> Result<Vec<TransferEdge>, DataError> {
    if !present {
        return Ok(Vec::new());
    }
    let mut reader = csv_reader(archive, path, "transfers.txt")?;
    let headers = reader
        .headers()
        .map_err(|error| csv_error(path, "transfers.txt", error))?
        .clone();
    let from = column(&headers, "from_stop_id", path, "transfers.txt")?;
    let to = column(&headers, "to_stop_id", path, "transfers.txt")?;
    let kind = headers.iter().position(|field| field == "transfer_type");
    let duration = headers
        .iter()
        .position(|field| field == "min_transfer_time");
    let mut edges = BTreeMap::<(u32, u32), u32>::new();
    for row in reader.records() {
        let row = row.map_err(|error| csv_error(path, "transfers.txt", error))?;
        let Some(from_index) = stop_indices.get(required(&row, from)).copied() else {
            continue;
        };
        let Some(to_index) = stop_indices.get(required(&row, to)).copied() else {
            continue;
        };
        if from_index == to_index {
            continue;
        }
        let transfer_type = optional(&row, kind).parse::<u8>().unwrap_or(0);
        if !matches!(transfer_type, 0..=2) {
            continue;
        }
        let seconds = optional(&row, duration)
            .parse::<u32>()
            .ok()
            .filter(|seconds| *seconds > 0)
            .unwrap_or(120);
        edges
            .entry((from_index, to_index))
            .and_modify(|current| *current = (*current).min(seconds))
            .or_insert(seconds);
    }
    Ok(edges
        .into_iter()
        .map(|((from, to), seconds)| {
            TransferEdge::new(
                stops[from as usize].id.clone(),
                stops[to as usize].id.clone(),
                seconds,
            )
        })
        .collect())
}

fn normalize_patterns(
    stops: &[TransitStop],
    routes: &[Route],
    trips: &[TripMeta],
    calendars: &HashMap<String, ServiceCalendar>,
    frequency_rows: &HashMap<u32, Vec<FrequencyRow>>,
    stop_times: &[CompactStopTime],
    shapes: &HashMap<String, Vec<ShapePoint>>,
) -> (
    Vec<TransitService>,
    Vec<ScheduledTrip>,
    Vec<TransitFrequency>,
    Vec<DerivedBusTravelTime>,
    (usize, usize, usize),
    (usize, Vec<String>),
) {
    let mut builders = Vec::<ServiceBuilder>::new();
    let mut service_indices = HashMap::<ServicePatternKey, usize>::new();
    let mut invalid_timing_trip_count = 0;
    let mut invalid_timing_trip_examples = Vec::new();
    let mut start = 0;
    while start < stop_times.len() {
        let trip_index = stop_times[start].trip_index;
        let mut end = start + 1;
        while end < stop_times.len() && stop_times[end].trip_index == trip_index {
            end += 1;
        }
        let trip = &trips[trip_index as usize];
        let rows = &stop_times[start..end];
        let mut ordered = rows.to_vec();
        ordered.sort_by_key(|row| row.sequence);
        let mut pattern = ordered.iter().map(|row| row.stop_index).collect::<Vec<_>>();
        pattern.dedup();
        if pattern.len() >= 2 && pattern.len() == ordered.len() {
            let key = ServicePatternKey {
                route_index: trip.route_index,
                direction: trip.direction,
                pattern: pattern.clone(),
            };
            let service_index = *service_indices.entry(key.clone()).or_insert_with(|| {
                builders.push(ServiceBuilder::new(&key));
                builders.len() - 1
            });
            let builder = &mut builders[service_index];
            builder.add_headsign(&trip.headsign);
            let calendar = calendars
                .get(&trip.gtfs_service_id)
                .cloned()
                .unwrap_or_default();
            let complete_times = ordered
                .iter()
                .all(|row| row.arrival_seconds.is_some() && row.departure_seconds.is_some());
            let template = ordered
                .iter()
                .map(|row| {
                    ScheduledStopTime::new(
                        row.arrival_seconds.unwrap_or_default(),
                        row.departure_seconds.unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            let chronological = complete_times && is_chronological_times(&template);
            if complete_times && !chronological {
                invalid_timing_trip_count += 1;
                if invalid_timing_trip_examples.len() < 20 {
                    invalid_timing_trip_examples.push(trip.id.clone());
                }
            }
            let has_frequencies = frequency_rows
                .get(&trip_index)
                .is_some_and(|frequency_rows| !frequency_rows.is_empty());
            if chronological && !has_frequencies && calendar_is_usable(&calendar) {
                let mut scheduled = ScheduledTrip::weekdays(
                    ScheduledTripId::new(trip.id.clone()),
                    TransitServiceId::new("pending"),
                    calendar.weekdays.clone(),
                    template.clone(),
                )
                .with_service_day_exceptions(calendar.added.clone(), calendar.removed.clone());
                if let (Some(start), Some(end)) = (calendar.start_date, calendar.end_date) {
                    scheduled = scheduled.with_service_date_range(start, end);
                }
                builder.scheduled_trips.push(scheduled);
                builder.has_exact_timing = true;
            }
            if let Some(rows) = frequency_rows.get(&trip_index) {
                if chronological && calendar_is_usable(&calendar) {
                    for (frequency_index, frequency) in rows.iter().enumerate() {
                        let mut normalized = TransitFrequency {
                            id: ScheduledTripId::new(format!(
                                "{}::frequency={frequency_index}",
                                trip.id
                            )),
                            service_id: TransitServiceId::new("pending"),
                            start_seconds: frequency.start_seconds,
                            end_seconds: frequency.end_seconds,
                            headway_seconds: frequency.headway_seconds,
                            exact_times: frequency.exact_times,
                            active_weekdays: calendar.weekdays.clone(),
                            start_date: calendar.start_date,
                            end_date: calendar.end_date,
                            added_service_days: calendar.added.clone(),
                            removed_service_days: calendar.removed.clone(),
                            template_stop_times: template.clone(),
                        };
                        normalized.service_id =
                            TransitServiceId::new(format!("pending:{service_index}"));
                        builder.frequencies.push(normalized);
                        builder.has_exact_timing = true;
                    }
                }
            }
            if routes[trip.route_index].mode == TransitMode::Bus {
                if let (Some(first_departure), Some(last_arrival)) = (
                    ordered.first().and_then(|row| row.departure_seconds),
                    ordered.last().and_then(|row| row.arrival_seconds),
                ) {
                    if last_arrival > first_departure {
                        let distances = trip_segment_distances(stops, &ordered, trip, shapes);
                        if let Some(samples) =
                            distribute_runtime(last_arrival - first_departure, &distances)
                        {
                            for (segment, seconds) in
                                builder.bus_segment_samples.iter_mut().zip(samples)
                            {
                                segment.push(seconds);
                            }
                        }
                    }
                }
            }
        }
        start = end;
    }

    let mut groups = BTreeMap::<(usize, Option<u8>), Vec<usize>>::new();
    for (index, builder) in builders.iter().enumerate() {
        groups
            .entry((builder.route_index, builder.direction))
            .or_default()
            .push(index);
    }
    let mut service_ids = vec![TransitServiceId::new(""); builders.len()];
    let mut services = Vec::with_capacity(builders.len());
    for ((route_index, direction), indices) in groups {
        let route = &routes[route_index];
        let mut indices = indices;
        indices.sort_by(|left, right| builders[*left].pattern.cmp(&builders[*right].pattern));
        let base = canonical_service_id(route, direction);
        for (position, builder_index) in indices.iter().enumerate() {
            let builder = &builders[*builder_index];
            let id = if position == 0 {
                base.clone()
            } else {
                TransitServiceId::new(format!("{}::pattern={position}", base.as_str()))
            };
            service_ids[*builder_index] = id.clone();
            let pattern = builder
                .pattern
                .iter()
                .map(|index| stops[*index as usize].id.clone())
                .collect();
            let scope = bus_realtime_scope(route, &builder.pattern, stops);
            let mut service = TransitService::new(
                id,
                route.mode,
                route.label.clone(),
                builder.headsign(route),
                pattern,
            );
            if let Some(scope) = scope {
                service = service.with_realtime_scope(scope);
            }
            services.push(service);
        }
    }

    let mut scheduled_trips = Vec::new();
    let mut normalized_frequencies = Vec::new();
    let mut derived_bus_times = Vec::new();
    let mut exact_count = 0;
    let mut derived_count = 0;
    let mut geometric_count = 0;
    for (index, builder) in builders.into_iter().enumerate() {
        let service_id = service_ids[index].clone();
        for mut trip in builder.scheduled_trips {
            trip.service_id = service_id.clone();
            scheduled_trips.push(trip);
        }
        for mut frequency in builder.frequencies {
            frequency.service_id = service_id.clone();
            normalized_frequencies.push(frequency);
        }
        if builder.has_exact_timing {
            exact_count += 1;
        } else if routes[builder.route_index].mode == TransitMode::Bus {
            let medians = builder
                .bus_segment_samples
                .into_iter()
                .map(median)
                .collect::<Option<Vec<_>>>();
            if let Some(adjacent_seconds) = medians {
                if !adjacent_seconds.is_empty() && adjacent_seconds.iter().all(|n| *n > 0) {
                    derived_bus_times.push(DerivedBusTravelTime {
                        service_id,
                        adjacent_seconds,
                    });
                    derived_count += 1;
                    continue;
                }
            }
            geometric_count += 1;
        } else {
            geometric_count += 1;
        }
    }
    (
        services,
        scheduled_trips,
        normalized_frequencies,
        derived_bus_times,
        (exact_count, derived_count, geometric_count),
        (invalid_timing_trip_count, invalid_timing_trip_examples),
    )
}

fn trip_segment_distances(
    stops: &[TransitStop],
    rows: &[CompactStopTime],
    trip: &TripMeta,
    shapes: &HashMap<String, Vec<ShapePoint>>,
) -> Vec<f64> {
    let explicit = rows
        .iter()
        .map(|row| row.shape_distance)
        .collect::<Option<Vec<_>>>();
    if let Some(values) = explicit {
        let distances = values
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>();
        if valid_distances(&distances) {
            return distances;
        }
    }
    if let Some(shape) = trip.shape_id.as_ref().and_then(|id| shapes.get(id)) {
        if let Some(distances) = stop_distances_on_shape(stops, rows, shape) {
            return distances;
        }
    }
    rows.windows(2)
        .map(|pair| {
            let from = stops[pair[0].stop_index as usize].coordinate;
            let to = stops[pair[1].stop_index as usize].coordinate;
            from.haversine_distance_metres(to)
        })
        .collect()
}

fn stop_distances_on_shape(
    stops: &[TransitStop],
    rows: &[CompactStopTime],
    shape: &[ShapePoint],
) -> Option<Vec<f64>> {
    if shape.len() < 2 {
        return None;
    }
    let mut cursor = 0;
    let mut distances = Vec::with_capacity(rows.len());
    for row in rows {
        let stop = stops[row.stop_index as usize].coordinate;
        let (index, _) =
            shape
                .iter()
                .enumerate()
                .skip(cursor)
                .min_by(|(_, left), (_, right)| {
                    point_distance(stop, left.latitude, left.longitude).total_cmp(&point_distance(
                        stop,
                        right.latitude,
                        right.longitude,
                    ))
                })?;
        cursor = index;
        distances.push(shape[index].distance);
    }
    let segment_distances = distances
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect::<Vec<_>>();
    valid_distances(&segment_distances).then_some(segment_distances)
}

fn distribute_runtime(runtime: u32, distances: &[f64]) -> Option<Vec<u32>> {
    if runtime == 0 || distances.is_empty() || !valid_distances(distances) {
        return None;
    }
    let total_distance: f64 = distances.iter().sum();
    if !total_distance.is_finite() || total_distance <= 0.0 || runtime < distances.len() as u32 {
        return None;
    }
    let mut segments = distances
        .iter()
        .map(|distance| ((f64::from(runtime) * distance / total_distance).floor() as u32).max(1))
        .collect::<Vec<_>>();
    let allocated: u32 = segments.iter().sum();
    if allocated < runtime {
        let mut fractions = distances
            .iter()
            .enumerate()
            .map(|(index, distance)| {
                let exact = f64::from(runtime) * distance / total_distance;
                (index, exact - exact.floor())
            })
            .collect::<Vec<_>>();
        fractions.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
        for (index, _) in fractions.into_iter().take((runtime - allocated) as usize) {
            segments[index] += 1;
        }
    } else if allocated > runtime {
        let mut surplus = allocated - runtime;
        for value in segments.iter_mut().rev() {
            while surplus > 0 && *value > 1 {
                *value -= 1;
                surplus -= 1;
            }
        }
    }
    Some(segments)
}

fn median(mut values: Vec<u32>) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle])
    } else {
        Some(((u64::from(values[middle - 1]) + u64::from(values[middle])) / 2) as u32)
    }
}

fn is_chronological_times(times: &[ScheduledStopTime]) -> bool {
    !times.is_empty()
        && times
            .iter()
            .all(|time| time.arrival_seconds <= time.departure_seconds)
        && times
            .windows(2)
            .all(|window| window[0].departure_seconds <= window[1].arrival_seconds)
}

fn valid_distances(distances: &[f64]) -> bool {
    !distances.is_empty()
        && distances
            .iter()
            .all(|distance| distance.is_finite() && *distance > 0.0)
}

fn canonical_service_id(route: &Route, direction: Option<u8>) -> TransitServiceId {
    if route.mode == TransitMode::Bus {
        if let Some(direction) = direction {
            let (base_id, encoded_direction) =
                directional_route_suffix(&route.id).unwrap_or((&route.id, direction));
            let route_id = if encoded_direction == direction {
                base_id
            } else {
                route.id.as_str()
            };
            return transit_service_id_for_direction(route_id, direction);
        }
        return TransitServiceId::new(route.id.clone());
    }
    if route.mode == TransitMode::Metro {
        let (line, route_direction, subroute) = metro_route_parts(&route.id, direction);
        return TransitServiceId::new(format!(
            "metro:{line}::direction={route_direction}::route={subroute}"
        ));
    }
    if matches!(route.mode, TransitMode::Rail | TransitMode::HighSpeedRail)
        && let Some(direction) = direction
        && !route.id.ends_with(&format!("_{direction}"))
    {
        return transit_service_id_for_direction(&route.id, direction);
    }
    TransitServiceId::new(route.id.clone())
}

fn direction_from_route_id(route_id: &str) -> Option<u8> {
    directional_route_suffix(route_id).map(|(_, direction)| direction)
}

fn directional_route_suffix(route_id: &str) -> Option<(&str, u8)> {
    let (base, suffix) = route_id.rsplit_once('_')?;
    let direction = suffix
        .parse::<u8>()
        .ok()
        .filter(|direction| *direction <= 1)?;
    Some((base, direction))
}

fn metro_route_parts(route_id: &str, direction: Option<u8>) -> (String, u8, String) {
    let (without_operator, operator) = route_id
        .split_once('_')
        .map(|(operator, rest)| (rest, operator))
        .unwrap_or((route_id, ""));
    let mut parts = without_operator.rsplitn(2, '_');
    let suffix = parts.next().unwrap_or("");
    let prefix = parts.next().unwrap_or(without_operator);
    let (route_direction, subroute) = match suffix.parse::<u8>() {
        Ok(route_direction) => (direction.unwrap_or(route_direction), prefix.to_owned()),
        Err(_) => (direction.unwrap_or(0), without_operator.to_owned()),
    };
    let line = if operator == "TRTC" {
        subroute.split('_').next().unwrap_or(&subroute).to_owned()
    } else {
        format!("{operator}_{subroute}")
    };
    (line, route_direction, subroute)
}

fn bus_realtime_scope(route: &Route, pattern: &[u32], stops: &[TransitStop]) -> Option<String> {
    if route.mode != TransitMode::Bus {
        return None;
    }
    let route_prefix = authority_prefix(&route.id);
    let agency_prefix = authority_prefix(&route.agency_id);
    let scope = city_scope(route_prefix).or_else(|| city_scope(agency_prefix));
    scope.map(str::to_owned).or_else(|| {
        (!pattern.is_empty()
            && pattern.iter().all(|index| {
                stops.get(*index as usize).is_some_and(|stop| {
                    (24.8..=25.4).contains(&stop.coordinate.latitude)
                        && (121.2..=122.1).contains(&stop.coordinate.longitude)
                })
            }))
        .then(|| "Taipei".to_owned())
    })
}

fn authority_prefix(value: &str) -> &str {
    let length = value
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .take(3)
        .map(char::len_utf8)
        .sum();
    &value[..length]
}

fn city_scope(route_prefix: &str) -> Option<&'static str> {
    match route_prefix {
        "TPE" => Some("Taipei"),
        "NWT" => Some("NewTaipei"),
        "TAO" => Some("Taoyuan"),
        "HSZ" | "HSQ" => Some("Hsinchu"),
        "MIA" => Some("Miaoli"),
        "TXG" => Some("Taichung"),
        "CHW" => Some("Changhua"),
        "NTO" => Some("Nantou"),
        "YUN" => Some("Yunlin"),
        "CYI" => Some("Chiayi"),
        "CYQ" => Some("ChiayiCounty"),
        "TNN" => Some("Tainan"),
        "KHH" => Some("Kaohsiung"),
        "PIF" => Some("Pingtung"),
        "ILA" => Some("YilanCounty"),
        "HUA" => Some("HualienCounty"),
        "TTT" => Some("TaitungCounty"),
        "KEE" => Some("Keelung"),
        "PEN" => Some("PenghuCounty"),
        "KIN" => Some("KinmenCounty"),
        "LIE" => Some("LienchiangCounty"),
        _ => None,
    }
}

fn is_tdx_stop_id(id: &str) -> bool {
    [
        "THB", "TPE", "NWT", "TAO", "HSZ", "HSQ", "MIA", "TXG", "CHW", "NTO", "YUN", "CYI", "CYQ",
        "TNN", "KHH", "PIF", "ILA", "HUA", "TTT", "KEE", "PEN", "KIN", "LIE", "TRTC-", "TRA-",
        "THSR-", "NTMC-", "TYMC-", "TMRT-", "KRTC-", "KLRT-",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix))
}

fn is_tdx_service_id(service: &TransitService, _routes: &[Route]) -> bool {
    match service.transit_mode {
        TransitMode::Bus => !service.id.as_str().contains("::pattern="),
        TransitMode::Metro => service
            .id
            .as_str()
            .strip_prefix("metro:")
            .and_then(|id| id.split_once("::direction=").map(|(line, _)| line))
            .is_some_and(|line| matches!(line, "BL" | "BR" | "G" | "O" | "R")),
        TransitMode::Rail | TransitMode::HighSpeedRail => false,
    }
}

fn tdx_stop_id(id: &str) -> String {
    for prefix in [
        "TRTC", "TRA", "THSR", "NTMC", "TYMC", "TMRT", "KRTC", "KLRT",
    ] {
        if let Some(suffix) = id.strip_prefix(&format!("{prefix}_")) {
            return format!("{prefix}-{suffix}");
        }
    }
    id.to_owned()
}

fn parse_route_mode(route_type: &str, route_id: &str) -> Option<TransitMode> {
    match route_type {
        "1" => Some(TransitMode::Metro),
        "2" if route_id.starts_with("THSR") => Some(TransitMode::HighSpeedRail),
        "2" => Some(TransitMode::Rail),
        "3" => Some(TransitMode::Bus),
        _ => None,
    }
}

fn mode_name(mode: TransitMode) -> &'static str {
    match mode {
        TransitMode::Bus => "bus",
        TransitMode::Metro => "metro",
        TransitMode::Rail => "rail",
        TransitMode::HighSpeedRail => "high_speed_rail",
    }
}

fn calendar_is_usable(calendar: &ServiceCalendar) -> bool {
    !calendar.weekdays.is_empty() || !calendar.added.is_empty()
}

fn parse_gtfs_time(value: &str) -> Option<u32> {
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

fn parse_gtfs_date(value: &str) -> Option<Date> {
    if value.len() != 8 {
        return None;
    }
    let year = value.get(..4)?.parse::<i32>().ok()?;
    let month = Month::try_from(value.get(4..6)?.parse::<u8>().ok()?).ok()?;
    let day = value.get(6..8)?.parse::<u8>().ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

fn weekday_for_index(index: usize) -> Weekday {
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

fn geographic_distance_metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let radians = std::f64::consts::PI / 180.0;
    let (lat1, lat2) = (lat1 * radians, lat2 * radians);
    let delta_latitude = lat2 - lat1;
    let delta_longitude = (lon2 - lon1) * radians;
    let a = (delta_latitude / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * (delta_longitude / 2.0).sin().powi(2);
    6_371_000.0 * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

fn point_distance(stop: RequestCoordinate, latitude: f64, longitude: f64) -> f64 {
    stop.haversine_distance_metres(RequestCoordinate {
        latitude,
        longitude,
    })
}

fn hash_file(path: &Path) -> Result<String, DataError> {
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

fn csv_reader<'a>(
    archive: &'a mut ZipArchive<File>,
    path: &Path,
    name: &str,
) -> Result<Reader<ZipFile<'a>>, DataError> {
    let file = archive
        .by_name(name)
        .map_err(|error| invalid_archive(path, format!("could not read {name}: {error}")))?;
    Ok(csv::ReaderBuilder::new().flexible(true).from_reader(file))
}

fn column(
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

fn required(row: &StringRecord, index: usize) -> &str {
    row.get(index).unwrap_or("")
}

fn optional(row: &StringRecord, index: Option<usize>) -> &str {
    index.and_then(|index| row.get(index)).unwrap_or("")
}

fn invalid_archive(path: &Path, error: impl std::fmt::Display) -> DataError {
    DataError::InvalidGtfsArchive {
        path: path.into(),
        detail: error.to_string(),
    }
}

fn csv_error(path: &Path, name: &'static str, error: csv::Error) -> DataError {
    DataError::MalformedGtfs {
        path: path.into(),
        kind: name,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write, path::PathBuf};

    use time::macros::date;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::{
        build_from_gtfs_archive, distribute_runtime, is_chronological_times, parse_gtfs_date,
        parse_gtfs_time, parse_route_mode, tdx_stop_id,
    };
    use crate::domain::transit::{ScheduledStopTime, TransitMode};

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
                "\u{feff}route_id,service_id,trip_id,direction_id,trip_headsign,shape_id\nTHB123_0,weekdays,bus-1,0,Bus end,bus-shape\nTHB123_0,weekdays,bus-2,0,Bus end,bus-shape\nTRTC_BL_BL-1_0,weekdays,metro-frequency,0,Blue end,\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence,shape_dist_traveled\nbus-1,,08:00:00,TPE001,1,0\nbus-1,,,TPE002,2,2\nbus-1,08:30:00,,TPE003,3,10\nbus-2,,09:00:00,TPE001,1,0\nbus-2,,,TPE002,2,2\nbus-2,09:40:00,,TPE003,3,10\nmetro-frequency,08:00:00,08:00:00,TRTC_BL12,1,\nmetro-frequency,08:04:00,08:04:00,TRTC_BL13,2,\nmetro-frequency,08:07:00,08:07:00,TRTC_BL14,3,\n",
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

    #[test]
    fn builds_network_timing_calendars_frequencies_and_transfers_from_gtfs() {
        let input = temporary_path("input.zip");
        let output = temporary_path("output");
        write_gtfs_fixture(&input);

        let built = build_from_gtfs_archive(&input, &output).unwrap();
        let network = &built.build.network;
        assert_eq!(built.diagnostics.stops_loaded, 6);
        assert_eq!(built.diagnostics.trips_loaded, 3);
        assert_eq!(built.diagnostics.frequencies_loaded, 1);
        assert_eq!(built.diagnostics.transfer_edges_loaded, 1);
        assert_eq!(built.diagnostics.invalid_timing_trips, 0);
        assert_eq!(built.diagnostics.exact_static_timing_services, 1);
        assert_eq!(built.diagnostics.derived_bus_timing_services, 1);
        assert_eq!(built.diagnostics.geometric_fallback_services, 0);
        assert_eq!(built.diagnostics.unresolved_bus_realtime_scope_services, 0);
        assert_eq!(built.diagnostics.gtfs_service_ids_without_tdx_mapping, 0);
        assert!(
            network.trips.is_empty(),
            "sparse Bus stop times are not exact trips"
        );
        assert_eq!(network.frequencies.len(), 1);
        assert_eq!(
            network.derived_bus_travel_times[0].adjacent_seconds,
            vec![420, 1680]
        );
        assert_eq!(network.transfer_edges[0].duration_seconds, 240);
        assert!(network.frequencies[0].runs_on(date!(2026 - 09 - 22)));
        assert!(!network.frequencies[0].runs_on(date!(2026 - 09 - 23)));
        assert!(network.frequencies[0].runs_on(date!(2026 - 09 - 24)));
        assert!(
            network
                .service(&crate::domain::ids::TransitServiceId::new(
                    "metro:BL::direction=0::route=BL_BL-1"
                ))
                .is_some()
        );
        assert!(
            network
                .service(&crate::domain::ids::TransitServiceId::new(
                    "THB123::direction=0"
                ))
                .is_some()
        );

        std::fs::remove_file(input).unwrap();
        std::fs::remove_dir_all(output).unwrap();
    }
}
