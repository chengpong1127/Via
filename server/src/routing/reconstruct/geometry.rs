use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use time::{OffsetDateTime, UtcOffset};

use crate::{
    domain::journey::{Geometry, Guidance, Leg, PlanningError},
    domain::location::GeoCoordinate,
    street::{Coordinate, Maneuver, ManeuverKind},
};

pub(super) fn overview_geometry(legs: &[Leg]) -> Geometry {
    let mut points = Vec::new();
    for leg in legs {
        let fallback = || {
            vec![
                Coordinate::try_new(leg.from.coordinate.latitude, leg.from.coordinate.longitude)
                    .expect("validated coordinate"),
                Coordinate::try_new(leg.to.coordinate.latitude, leg.to.coordinate.longitude)
                    .expect("validated coordinate"),
            ]
        };
        for point in decode_polyline6(&leg.geometry.points).unwrap_or_else(fallback) {
            if points.last() != Some(&point) {
                points.push(point);
            }
        }
    }
    Geometry {
        encoding: "polyline6",
        points: encode_polyline6(&points),
    }
}

pub(super) fn straight_geometry(from: GeoCoordinate, to: GeoCoordinate) -> Geometry {
    let from = Coordinate::try_new(from.latitude, from.longitude).expect("validated coordinate");
    let to = Coordinate::try_new(to.latitude, to.longitude).expect("validated coordinate");
    Geometry {
        encoding: "polyline6",
        points: encode_polyline6(&[from, to]),
    }
}

pub(super) fn guidance(maneuvers: &[Maneuver]) -> Vec<Guidance> {
    maneuvers
        .iter()
        .map(|maneuver| Guidance {
            code: match maneuver.kind {
                ManeuverKind::Depart => "depart",
                ManeuverKind::Continue => "continue",
                ManeuverKind::TurnLeft => "turn_left",
                ManeuverKind::TurnRight => "turn_right",
                ManeuverKind::UTurn => "u_turn",
                ManeuverKind::EnterRoundabout => "enter_roundabout",
                ManeuverKind::ExitRoundabout => "exit_roundabout",
                ManeuverKind::Arrive => "arrive",
            },
            parameters: BTreeMap::new(),
        })
        .collect()
}

pub(super) fn encode_polyline6(coordinates: &[Coordinate]) -> String {
    let mut output = String::new();
    let mut previous_latitude = 0_i64;
    let mut previous_longitude = 0_i64;
    for coordinate in coordinates {
        let latitude = (coordinate.latitude() * 1_000_000.0).round() as i64;
        let longitude = (coordinate.longitude() * 1_000_000.0).round() as i64;
        encode_polyline_value(latitude - previous_latitude, &mut output);
        encode_polyline_value(longitude - previous_longitude, &mut output);
        previous_latitude = latitude;
        previous_longitude = longitude;
    }
    output
}

fn encode_polyline_value(value: i64, output: &mut String) {
    let mut encoded = if value < 0 {
        (!(value << 1)) as u64
    } else {
        (value << 1) as u64
    };
    while encoded >= 0x20 {
        output.push(char::from_u32(((0x20 | (encoded & 0x1f)) + 63) as u32).unwrap());
        encoded >>= 5;
    }
    output.push(char::from_u32((encoded + 63) as u32).unwrap());
}

fn decode_polyline6(encoded: &str) -> Option<Vec<Coordinate>> {
    let mut values = Vec::new();
    let mut current = 0_i64;
    let mut shift = 0_u32;
    for byte in encoded.bytes() {
        if !(63..=126).contains(&byte) {
            return None;
        }
        let value = i64::from(byte - 63);
        current |= (value & 0x1f) << shift;
        if value < 0x20 {
            values.push(if current & 1 == 1 {
                !(current >> 1)
            } else {
                current >> 1
            });
            current = 0;
            shift = 0;
        } else {
            shift += 5;
            if shift > 30 {
                return None;
            }
        }
    }
    if shift != 0 || values.len() % 2 != 0 {
        return None;
    }
    let mut latitude = 0_i64;
    let mut longitude = 0_i64;
    let mut points = Vec::with_capacity(values.len() / 2);
    for pair in values.chunks_exact(2) {
        latitude += pair[0];
        longitude += pair[1];
        points.push(
            Coordinate::try_new(
                latitude as f64 / 1_000_000.0,
                longitude as f64 / 1_000_000.0,
            )
            .ok()?,
        );
    }
    Some(points)
}

pub(super) fn short_digest(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("match_{:x}", digest.finalize())[..22].to_owned()
}

pub(in crate::routing) fn to_street_coordinate(
    coordinate: GeoCoordinate,
) -> Result<Coordinate, PlanningError> {
    Coordinate::try_from(coordinate).map_err(|_| PlanningError::Internal)
}

pub(in crate::routing) fn taipei_now() -> OffsetDateTime {
    OffsetDateTime::now_utc().to_offset(UtcOffset::from_hms(8, 0, 0).unwrap())
}
