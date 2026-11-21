use time::{format_description::FormatItem, macros::format_description};

use serde::{Deserialize, Serialize};

use crate::street::{
    Attribution, Coordinate, Maneuver, ManeuverKind, MatrixCell, Metres, RoutingConstraints,
    Seconds, StreetRoutingError, StreetTravelMode,
};

const OSM_ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";
const ODBL_URL: &str = "https://opendatacommons.org/licenses/odbl/1-0/";

const VALHALLA_DEPART_AT: u8 = 1;
const VALHALLA_LOCAL_TIME_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]");

pub(super) fn wire_time(
    constraints: RoutingConstraints,
) -> Result<Option<DateTimeRequest>, StreetRoutingError> {
    constraints
        .time
        .map(|routing_time| {
            let value = routing_time
                .local_date_time
                .format(VALHALLA_LOCAL_TIME_FORMAT)
                .map_err(|_| StreetRoutingError::InvalidRequest {
                    reason: "street-routing time could not be formatted".into(),
                })?;
            Ok(DateTimeRequest {
                kind: VALHALLA_DEPART_AT,
                value,
            })
        })
        .transpose()
}

pub(super) fn osm_attribution() -> Vec<Attribution> {
    vec![Attribution {
        text: "© OpenStreetMap contributors".into(),
        url: OSM_ATTRIBUTION_URL.into(),
        licence: ODBL_URL.into(),
    }]
}

pub(super) fn seconds(value: f64) -> Result<Seconds, StreetRoutingError> {
    if !value.is_finite() || value < 0.0 || value > f64::from(u32::MAX) {
        return Err(StreetRoutingError::MalformedResponse);
    }
    Ok(Seconds(value.round() as u32))
}

pub(super) fn kilometres_to_metres(value: f64) -> Result<Metres, StreetRoutingError> {
    let metres = value * 1_000.0;
    if !metres.is_finite() || metres < 0.0 || metres > f64::from(u32::MAX) {
        return Err(StreetRoutingError::MalformedResponse);
    }
    Ok(Metres(metres.round() as u32))
}

pub(super) fn normalize_matrix_cell(
    wire: MatrixWireCell,
) -> Result<MatrixCell, StreetRoutingError> {
    match (wire.time, wire.distance) {
        (Some(time), Some(distance)) => Ok(MatrixCell::Reachable {
            duration: seconds(time)?,
            distance: kilometres_to_metres(distance)?,
        }),
        (None, None) => Ok(MatrixCell::Unreachable),
        _ => Err(StreetRoutingError::MalformedResponse),
    }
}

pub(super) fn normalize_maneuver(wire: RouteManeuver) -> Result<Maneuver, StreetRoutingError> {
    let kind = match wire.kind {
        1..=3 => ManeuverKind::Depart,
        4..=6 => ManeuverKind::Arrive,
        9..=11 | 18 | 20 | 23 => ManeuverKind::TurnRight,
        12 | 13 => ManeuverKind::UTurn,
        14..=16 | 19 | 21 | 24 => ManeuverKind::TurnLeft,
        26 => ManeuverKind::EnterRoundabout,
        27 => ManeuverKind::ExitRoundabout,
        _ => ManeuverKind::Continue,
    };
    Ok(Maneuver {
        kind,
        begin_geometry_index: wire.begin_shape_index,
        end_geometry_index: wire.end_shape_index,
        street_names: wire.street_names.unwrap_or_default(),
        roundabout_exit: wire.roundabout_exit_count,
    })
}

pub(super) fn decode_polyline6(encoded: &str) -> Result<Vec<Coordinate>, StreetRoutingError> {
    let bytes = encoded.as_bytes();
    let mut index = 0;
    let mut latitude = 0_i64;
    let mut longitude = 0_i64;
    let mut coordinates = Vec::new();
    while index < bytes.len() {
        latitude = latitude
            .checked_add(decode_component(bytes, &mut index)?)
            .ok_or(StreetRoutingError::MalformedResponse)?;
        longitude = longitude
            .checked_add(decode_component(bytes, &mut index)?)
            .ok_or(StreetRoutingError::MalformedResponse)?;
        coordinates.push(
            Coordinate::try_new(
                latitude as f64 / 1_000_000.0,
                longitude as f64 / 1_000_000.0,
            )
            .map_err(|_| StreetRoutingError::MalformedResponse)?,
        );
    }
    Ok(coordinates)
}

fn decode_component(bytes: &[u8], index: &mut usize) -> Result<i64, StreetRoutingError> {
    let mut result = 0_u64;
    let mut shift = 0_u32;
    loop {
        let byte = *bytes
            .get(*index)
            .ok_or(StreetRoutingError::MalformedResponse)?;
        *index += 1;
        if !(63..=126).contains(&byte) || shift > 60 {
            return Err(StreetRoutingError::MalformedResponse);
        }
        let value = u64::from(byte - 63);
        result |= (value & 0x1f) << shift;
        if value < 0x20 {
            break;
        }
        shift += 5;
    }
    let signed = if result & 1 == 1 {
        !(result >> 1) as i64
    } else {
        (result >> 1) as i64
    };
    Ok(signed)
}

#[derive(Clone, Copy, Serialize)]
pub(super) struct WireLocation {
    pub(super) lat: f64,
    pub(super) lon: f64,
}

impl From<Coordinate> for WireLocation {
    fn from(coordinate: Coordinate) -> Self {
        Self {
            lat: coordinate.latitude(),
            lon: coordinate.longitude(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct MatrixRequest {
    pub(super) sources: Vec<WireLocation>,
    pub(super) targets: Vec<WireLocation>,
    pub(super) costing: &'static str,
    pub(super) units: &'static str,
    pub(super) verbose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) date_time: Option<DateTimeRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) expansion_max_distance: Option<u32>,
}

#[derive(Serialize)]
pub(super) struct DateTimeRequest {
    #[serde(rename = "type")]
    pub(super) kind: u8,
    pub(super) value: String,
}

#[derive(Deserialize)]
pub(super) struct MatrixResponse {
    pub(super) sources_to_targets: Vec<Vec<MatrixWireCell>>,
}

#[derive(Deserialize)]
pub(super) struct MatrixWireCell {
    pub(super) time: Option<f64>,
    pub(super) distance: Option<f64>,
}

#[derive(Serialize)]
pub(super) struct RouteRequest {
    pub(super) locations: Vec<WireLocation>,
    pub(super) costing: &'static str,
    pub(super) units: &'static str,
    pub(super) shape_format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) date_time: Option<DateTimeRequest>,
}

#[derive(Deserialize)]
pub(super) struct RouteResponse {
    pub(super) trip: RouteTrip,
}

#[derive(Deserialize)]
pub(super) struct RouteTrip {
    pub(super) summary: RouteSummary,
    pub(super) legs: Vec<RouteLeg>,
}

#[derive(Deserialize)]
pub(super) struct RouteSummary {
    pub(super) time: f64,
    pub(super) length: f64,
}

#[derive(Deserialize)]
pub(super) struct RouteLeg {
    pub(super) shape: String,
    pub(super) maneuvers: Vec<RouteManeuver>,
}

#[derive(Deserialize)]
pub(super) struct RouteManeuver {
    #[serde(rename = "type")]
    pub(super) kind: u16,
    pub(super) begin_shape_index: usize,
    pub(super) end_shape_index: Option<usize>,
    pub(super) street_names: Option<Vec<String>>,
    pub(super) roundabout_exit_count: Option<u32>,
}

pub(super) fn costing(travel_mode: StreetTravelMode) -> &'static str {
    match travel_mode {
        StreetTravelMode::Walking => "pedestrian",
        StreetTravelMode::Bicycle => "bicycle",
    }
}

#[derive(Deserialize)]
pub(super) struct StatusResponse {
    pub(super) has_tiles: Option<bool>,
    pub(super) available_actions: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct ValhallaErrorResponse {
    pub(super) error_code: u16,
}
