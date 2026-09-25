//! Map normalized GTFS identities to TDX provider identities.

use std::collections::{HashMap, HashSet};

use gtfs_structures::{RawTrip, Route};

use crate::{
    domain::{
        ids::{TransitPatternId, TransitTripId},
        transit::{TransitMode, TransitNetwork, TransitStop},
    },
    providers::bindings::{
        BusPatternBinding, MetroPatternBinding, TdxProviderBindings, TraTripBinding,
    },
    tdx::transit_service_id_for_direction,
};

use super::GtfsBuildDiagnostics;

pub(super) struct BindingInputs {
    pub(super) routes: Vec<Route>,
    pub(super) route_modes: HashMap<String, TransitMode>,
    pub(super) trips: Vec<RawTrip>,
}

pub(super) fn via_route_id(route: &Route, mode: TransitMode) -> String {
    match mode {
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
    let suffix = parts.next().unwrap_or_default();
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

pub(super) fn build(
    inputs: &BindingInputs,
    network: &TransitNetwork,
    diagnostics: &mut GtfsBuildDiagnostics,
) -> TdxProviderBindings {
    let patterns = &network.patterns;
    let scheduled_trips = &network.trips;
    let stop_positions = network
        .stops
        .iter()
        .enumerate()
        .map(|(index, stop)| (stop.id.clone(), index as u32))
        .collect::<HashMap<_, _>>();
    let routes_by_id = inputs
        .routes
        .iter()
        .map(|route| (route.id.as_str(), route))
        .collect::<HashMap<_, _>>();
    let mut route_by_via_id = HashMap::<String, &Route>::new();
    let mut route_by_via_direction = HashMap::<(String, u8), &Route>::new();
    for route in &inputs.routes {
        let Some(mode) = inputs.route_modes.get(&route.id) else {
            continue;
        };
        let via_id = via_route_id(route, *mode);
        route_by_via_id.insert(via_id.clone(), route);
        if let Some(direction) = direction_from_route_id(&route.id) {
            route_by_via_direction.insert((via_id, direction), route);
        }
    }

    let mut bindings = TdxProviderBindings::default();
    for pattern in patterns {
        let route = pattern
            .direction
            .and_then(|direction| {
                route_by_via_direction.get(&(pattern.route_id.as_str().to_owned(), direction))
            })
            .or_else(|| route_by_via_id.get(pattern.route_id.as_str()));
        let Some(route) = route else {
            continue;
        };
        let mode = inputs.route_modes[route.id.as_str()];
        match mode {
            TransitMode::Bus => {
                let positions = pattern
                    .stops
                    .iter()
                    .filter_map(|id| stop_positions.get(id).copied())
                    .collect::<Vec<_>>();
                if let Some(realtime_scope) = bus_realtime_scope(route, &positions, &network.stops)
                {
                    bindings.bus_patterns.insert(
                        pattern.id.clone(),
                        BusPatternBinding {
                            tdx_service_id: canonical_service_id(route, pattern.direction, mode)
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
    let mut usable_trip_ids = HashSet::new();
    for trip in &inputs.trips {
        if !inputs.route_modes.contains_key(trip.route_id.as_str())
            || !usable_trip_ids.insert(trip.id.as_str())
            || !scheduled_ids.contains(trip.id.as_str())
        {
            continue;
        }
        let Some(route) = routes_by_id.get(trip.route_id.as_str()).copied() else {
            continue;
        };
        if inputs.route_modes.get(&route.id) == Some(&TransitMode::Rail)
            && let Some(train_number) = route
                .id
                .strip_prefix("TRA_")
                .and_then(|suffix| suffix.split('_').next())
            && !train_number.is_empty()
        {
            bindings.tra_trips.insert(
                TransitTripId::new(trip.id.clone()),
                TraTripBinding {
                    train_number: train_number.to_owned(),
                },
            );
        }
    }
    let unresolved_bus_scopes = patterns
        .iter()
        .filter(|pattern| {
            let route = route_by_via_id[pattern.route_id.as_str()];
            inputs.route_modes[route.id.as_str()] == TransitMode::Bus
                && bus_realtime_scope(
                    route,
                    &pattern
                        .stops
                        .iter()
                        .filter_map(|id| stop_positions.get(id).copied())
                        .collect::<Vec<_>>(),
                    &network.stops,
                )
                .is_none()
        })
        .map(|pattern| pattern.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.unresolved_bus_realtime_scope_patterns = unresolved_bus_scopes.len();
    diagnostics.unresolved_bus_realtime_scope_examples =
        unresolved_bus_scopes.into_iter().take(20).collect();

    let unmapped_stops = network
        .stops
        .iter()
        .filter(|stop| !is_tdx_stop_id(stop.id.as_str()))
        .map(|stop| stop.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.gtfs_stop_ids_without_tdx_mapping = unmapped_stops.len();
    diagnostics.unmapped_stop_id_examples = unmapped_stops.into_iter().take(20).collect();

    let unmapped_patterns = patterns
        .iter()
        .filter(|pattern| {
            !bindings.bus_patterns.contains_key(&pattern.id)
                && !bindings.metro_patterns.contains_key(&pattern.id)
                && !matches!(
                    inputs
                        .route_modes
                        .get(route_by_via_id[pattern.route_id.as_str()].id.as_str()),
                    Some(TransitMode::Rail | TransitMode::HighSpeedRail)
                )
        })
        .map(|pattern| pattern.id.as_str().to_owned())
        .collect::<Vec<_>>();
    diagnostics.gtfs_pattern_ids_without_tdx_mapping = unmapped_patterns.len();
    diagnostics.unmapped_pattern_id_examples = unmapped_patterns.into_iter().take(20).collect();

    bindings
}

fn canonical_service_id(
    route: &Route,
    direction: Option<u8>,
    mode: TransitMode,
) -> TransitPatternId {
    if mode == TransitMode::Bus {
        if let Some(direction) = direction {
            let (base_id, encoded_direction) =
                directional_route_suffix(&route.id).unwrap_or((&route.id, direction));
            let route_id = if encoded_direction == direction {
                base_id
            } else {
                &route.id
            };
            return transit_service_id_for_direction(route_id, direction);
        }
        return TransitPatternId::new(route.id.clone());
    }
    if mode == TransitMode::Metro {
        let (line, route_direction, subroute) = metro_route_parts(&route.id, direction);
        return TransitPatternId::new(format!(
            "metro:{line}::direction={route_direction}::route={subroute}"
        ));
    }
    if matches!(mode, TransitMode::Rail | TransitMode::HighSpeedRail)
        && let Some(direction) = direction
        && !route.id.ends_with(&format!("_{direction}"))
    {
        return transit_service_id_for_direction(&route.id, direction);
    }
    TransitPatternId::new(route.id.clone())
}

fn bus_realtime_scope(route: &Route, pattern: &[u32], stops: &[TransitStop]) -> Option<String> {
    let scope = city_scope(authority_prefix(&route.id)).or_else(|| {
        route
            .agency_id
            .as_deref()
            .map(authority_prefix)
            .and_then(city_scope)
    });
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
