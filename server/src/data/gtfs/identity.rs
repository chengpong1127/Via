//! GTFS identifier normalization and TDX provider binding helpers.

use std::collections::{HashMap, HashSet};

use crate::{
    domain::{
        ids::{BoardingPointId, TransitPatternId, TransitTripId},
        transit::{TransitMode, TransitPattern, TransitStop, TransitTrip},
    },
    providers::bindings::{
        BusPatternBinding, MetroPatternBinding, TdxProviderBindings, TraTripBinding,
    },
    tdx::transit_service_id_for_direction,
};

use super::routes::Route;

pub(super) fn provider_bindings<'a>(
    routes: &[Route],
    patterns: &[TransitPattern],
    trips: impl Iterator<Item = (&'a str, usize)>,
    scheduled_trips: &[TransitTrip],
    stop_position_by_id: &HashMap<BoardingPointId, u32>,
    stops: &[TransitStop],
    route_by_via_id: &HashMap<String, usize>,
) -> TdxProviderBindings {
    let route_by_via_direction = routes
        .iter()
        .enumerate()
        .filter_map(|(index, route)| {
            direction_from_route_id(&route.id)
                .map(|direction| ((via_route_id(route), direction), index))
        })
        .collect::<HashMap<_, _>>();
    let mut bindings = TdxProviderBindings::default();
    for pattern in patterns {
        let route_index = pattern
            .direction
            .and_then(|direction| {
                route_by_via_direction.get(&(pattern.route_id.as_str().to_owned(), direction))
            })
            .or_else(|| route_by_via_id.get(pattern.route_id.as_str()));
        let Some(route) = route_index.map(|index| &routes[*index]) else {
            continue;
        };
        match route.mode {
            TransitMode::Bus => {
                let positions = pattern
                    .stops
                    .iter()
                    .filter_map(|id| stop_position_by_id.get(id).copied())
                    .collect::<Vec<_>>();
                if let Some(realtime_scope) = bus_realtime_scope(route, &positions, stops) {
                    bindings.bus_patterns.insert(
                        pattern.id.clone(),
                        BusPatternBinding {
                            tdx_service_id: canonical_service_id(route, pattern.direction)
                                .as_str()
                                .to_owned(),
                            realtime_scope,
                        },
                    );
                }
            }
            TransitMode::Metro => {
                let (line_id, direction, _) = metro_route_parts(&route.id, pattern.direction);
                bindings.metro_patterns.insert(
                    pattern.id.clone(),
                    MetroPatternBinding { line_id, direction },
                );
            }
            TransitMode::Rail | TransitMode::HighSpeedRail => {}
        }
    }
    let scheduled_ids = scheduled_trips
        .iter()
        .map(|trip| trip.id.as_str())
        .collect::<HashSet<_>>();
    for (trip_id, route_index) in trips {
        if !scheduled_ids.contains(trip_id) {
            continue;
        }
        let route = &routes[route_index];
        if route.mode == TransitMode::Rail
            && let Some(train_number) = route
                .id
                .strip_prefix("TRA_")
                .and_then(|suffix| suffix.split('_').next())
            && !train_number.is_empty()
        {
            bindings.tra_trips.insert(
                TransitTripId::new(trip_id.to_owned()),
                TraTripBinding {
                    train_number: train_number.to_owned(),
                },
            );
        }
    }
    bindings
}

pub(super) fn canonical_service_id(route: &Route, direction: Option<u8>) -> TransitPatternId {
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
        return TransitPatternId::new(route.id.clone());
    }
    if route.mode == TransitMode::Metro {
        let (line, route_direction, subroute) = metro_route_parts(&route.id, direction);
        return TransitPatternId::new(format!(
            "metro:{line}::direction={route_direction}::route={subroute}"
        ));
    }
    if matches!(route.mode, TransitMode::Rail | TransitMode::HighSpeedRail)
        && let Some(direction) = direction
        && !route.id.ends_with(&format!("_{direction}"))
    {
        return transit_service_id_for_direction(&route.id, direction);
    }
    TransitPatternId::new(route.id.clone())
}

pub(super) fn via_route_id(route: &Route) -> String {
    match route.mode {
        TransitMode::Bus => directional_route_suffix(&route.id)
            .map_or_else(|| route.id.clone(), |(base, _)| base.to_owned()),
        TransitMode::Metro => {
            let (line, _, _) = metro_route_parts(&route.id, None);
            format!("metro:{line}")
        }
        TransitMode::Rail => "tra".to_owned(),
        TransitMode::HighSpeedRail => "thsr".to_owned(),
    }
}

pub(super) fn direction_from_route_id(route_id: &str) -> Option<u8> {
    directional_route_suffix(route_id).map(|(_, direction)| direction)
}

pub(super) fn directional_route_suffix(route_id: &str) -> Option<(&str, u8)> {
    let (base, suffix) = route_id.rsplit_once('_')?;
    let direction = suffix
        .parse::<u8>()
        .ok()
        .filter(|direction| *direction <= 1)?;
    Some((base, direction))
}

pub(super) fn metro_route_parts(route_id: &str, direction: Option<u8>) -> (String, u8, String) {
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

pub(super) fn bus_realtime_scope(
    route: &Route,
    pattern: &[u32],
    stops: &[TransitStop],
) -> Option<String> {
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

pub(super) fn authority_prefix(value: &str) -> &str {
    let length = value
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .take(3)
        .map(char::len_utf8)
        .sum();
    &value[..length]
}

pub(super) fn city_scope(route_prefix: &str) -> Option<&'static str> {
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

pub(super) fn is_tdx_stop_id(id: &str) -> bool {
    [
        "THB", "TPE", "NWT", "TAO", "HSZ", "HSQ", "MIA", "TXG", "CHW", "NTO", "YUN", "CYI", "CYQ",
        "TNN", "KHH", "PIF", "ILA", "HUA", "TTT", "KEE", "PEN", "KIN", "LIE", "TRTC-", "TRA-",
        "THSR-", "NTMC-", "TYMC-", "TMRT-", "KRTC-", "KLRT-",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix))
}

pub(super) fn tdx_stop_id(id: &str) -> String {
    for prefix in [
        "TRTC", "TRA", "THSR", "NTMC", "TYMC", "TMRT", "KRTC", "KLRT",
    ] {
        if let Some(suffix) = id.strip_prefix(&format!("{prefix}_")) {
            return format!("{prefix}-{suffix}");
        }
    }
    id.to_owned()
}
