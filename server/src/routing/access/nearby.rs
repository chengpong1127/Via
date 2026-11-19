use crate::{
    domain::location::GeoCoordinate,
    domain::{transit::network::TransitMode, transit::runtime::TransitRuntime},
    street::{Coordinate, MatrixCell},
};

use crate::routing::model::{EndpointStop, JourneyPlanningLimits};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreetMetric {
    pub duration_seconds: u32,
    pub distance_metres: u32,
}

#[derive(Clone)]
struct NearbyStopCandidate {
    stop_id: crate::domain::ids::BoardingPointId,
    coordinate: Coordinate,
    modes: Vec<TransitMode>,
}

pub(super) fn prune_coverage(
    transit: &TransitRuntime,
    stops: Vec<EndpointStop>,
    limit: usize,
) -> Vec<EndpointStop> {
    let mut retained_stops = Vec::with_capacity(stops.len());
    for stop in stops {
        if retained_stops.iter().any(|existing: &EndpointStop| {
            existing.walking_seconds <= stop.walking_seconds
                && stop_coverage_dominates(transit, &existing.stop_id, &stop.stop_id)
        }) {
            continue;
        }
        retained_stops.push(stop);
    }
    retained_stops.truncate(limit.max(1));
    retained_stops
}

fn stop_coverage_dominates(
    transit: &TransitRuntime,
    dominant_stop: &crate::domain::ids::BoardingPointId,
    candidate_stop: &crate::domain::ids::BoardingPointId,
) -> bool {
    transit
        .pattern_positions_at(candidate_stop)
        .iter()
        .all(|position| {
            transit
                .pattern_positions_at(dominant_stop)
                .iter()
                .any(|dominant| dominant.pattern_id == position.pattern_id)
        })
}

pub(super) fn nearby_stops(
    transit: &TransitRuntime,
    coordinate: Coordinate,
    allowed_transit_modes: &[TransitMode],
    limit: usize,
    limits: &JourneyPlanningLimits,
) -> Vec<(crate::domain::ids::BoardingPointId, Coordinate)> {
    let request_coordinate = GeoCoordinate {
        latitude: coordinate.latitude(),
        longitude: coordinate.longitude(),
    };
    let candidates = transit
        .stops_within_radius(request_coordinate, limits.max_walking_radius_metres)
        .into_iter()
        .filter_map(|stop| {
            let modes = modes_at_stop(transit, &stop.id);
            if !allowed_transit_modes
                .iter()
                .any(|mode| modes.contains(mode))
            {
                return None;
            }
            Coordinate::try_new(stop.coordinate.latitude, stop.coordinate.longitude)
                .ok()
                .map(|coordinate| NearbyStopCandidate {
                    stop_id: stop.id.clone(),
                    coordinate,
                    modes,
                })
        })
        .collect::<Vec<_>>();
    select_mode_balanced(candidates, allowed_transit_modes, limit)
        .into_iter()
        .map(|candidate| (candidate.stop_id, candidate.coordinate))
        .collect()
}

fn select_mode_balanced(
    candidates: Vec<NearbyStopCandidate>,
    allowed_transit_modes: &[TransitMode],
    limit: usize,
) -> Vec<NearbyStopCandidate> {
    let limit = limit.max(1);
    let mut selected_candidates = Vec::with_capacity(limit.min(candidates.len()));
    loop {
        let mut added = false;
        for mode in allowed_transit_modes {
            if selected_candidates.len() >= limit {
                break;
            }
            if let Some(candidate) = candidates.iter().find(|candidate| {
                candidate.modes.contains(mode)
                    && !selected_candidates
                        .iter()
                        .any(|known: &NearbyStopCandidate| known.stop_id == candidate.stop_id)
            }) {
                selected_candidates.push(candidate.clone());
                added = true;
            }
        }
        if !added || selected_candidates.len() >= limit {
            break;
        }
    }
    for candidate in candidates {
        if selected_candidates.len() >= limit {
            break;
        }
        if !selected_candidates
            .iter()
            .any(|known| known.stop_id == candidate.stop_id)
        {
            selected_candidates.push(candidate);
        }
    }
    selected_candidates
}

pub(crate) fn modes_at_stop(
    transit: &TransitRuntime,
    stop_id: &crate::domain::ids::BoardingPointId,
) -> Vec<TransitMode> {
    let mut modes = transit
        .pattern_positions_at(stop_id)
        .iter()
        .map(|position| transit.required_pattern(&position.pattern_id))
        .map(|pattern| transit.required_route(&pattern.route_id).mode)
        .collect::<Vec<_>>();
    modes.sort_unstable();
    modes.dedup();
    modes
}

pub(crate) fn street_metric(cell: &MatrixCell) -> Option<StreetMetric> {
    match cell {
        MatrixCell::Reachable { duration, distance } => Some(StreetMetric {
            duration_seconds: duration.0,
            distance_metres: distance.0,
        }),
        MatrixCell::Unreachable => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{JourneyPlanningLimits, nearby_stops, stop_coverage_dominates};
    use crate::{
        domain::location::GeoCoordinate,
        domain::{
            ids::BoardingPointId,
            transit::network::{TransitMode, TransitNetwork, TransitPattern, TransitStop},
            transit::runtime::TransitRuntime,
        },
        street::Coordinate,
    };

    fn stop(id: &str) -> TransitStop {
        TransitStop::new(
            BoardingPointId::new(id),
            id,
            GeoCoordinate {
                latitude: 25.0,
                longitude: 121.5,
            },
        )
    }

    fn stop_at(id: &str, coordinate: GeoCoordinate) -> TransitStop {
        TransitStop::new(BoardingPointId::new(id), id, coordinate)
    }

    #[test]
    fn coverage_dominance_keeps_unique_pattern_and_mode_stops() {
        let network = TransitNetwork::fixture(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![stop("a"), stop("b"), stop("c"), stop("d")],
            vec![
                TransitPattern::fixture(
                    crate::domain::ids::TransitPatternId::new("bus"),
                    TransitMode::Bus,
                    "bus",
                    "b",
                    vec![BoardingPointId::new("a"), BoardingPointId::new("b")],
                ),
                TransitPattern::fixture(
                    crate::domain::ids::TransitPatternId::new("metro"),
                    TransitMode::Metro,
                    "metro",
                    "d",
                    vec![BoardingPointId::new("c"), BoardingPointId::new("d")],
                ),
            ],
            Vec::new(),
        );

        let transit = TransitRuntime::new(std::sync::Arc::new(network));
        assert!(stop_coverage_dominates(
            &transit,
            &BoardingPointId::new("a"),
            &BoardingPointId::new("b")
        ));
        assert!(!stop_coverage_dominates(
            &transit,
            &BoardingPointId::new("a"),
            &BoardingPointId::new("c")
        ));
    }

    #[test]
    fn nearby_stops_reserves_a_transit_mode_before_the_nearest_stop_limit() {
        let origin = GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        };
        let metro_coordinate = GeoCoordinate {
            latitude: 25.0004,
            longitude: 121.5,
        };
        let mut stops = (0..24)
            .map(|index| stop_at(&format!("bus-stop-{index}"), origin))
            .collect::<Vec<_>>();
        stops.push(stop_at("metro-stop", metro_coordinate));
        stops.push(stop_at(
            "other-metro-stop",
            GeoCoordinate {
                latitude: 25.0005,
                longitude: 121.5,
            },
        ));
        let mut patterns = Vec::new();
        for index in 0..24 {
            patterns.push(TransitPattern::fixture(
                crate::domain::ids::TransitPatternId::new(format!("bus-{index}")),
                TransitMode::Bus,
                "bus",
                "bus",
                vec![
                    BoardingPointId::new(format!("bus-stop-{index}")),
                    BoardingPointId::new(format!("bus-end-{index}")),
                ],
            ));
        }
        stops.extend((0..24).map(|index| {
            stop_at(
                &format!("bus-end-{index}"),
                GeoCoordinate {
                    latitude: 25.1,
                    longitude: 121.5,
                },
            )
        }));
        patterns.push(TransitPattern::fixture(
            crate::domain::ids::TransitPatternId::new("red-line"),
            TransitMode::Metro,
            "red",
            "red",
            vec![
                BoardingPointId::new("metro-stop"),
                BoardingPointId::new("metro-end"),
            ],
        ));
        patterns.push(TransitPattern::fixture(
            crate::domain::ids::TransitPatternId::new("blue-line"),
            TransitMode::Metro,
            "blue",
            "blue",
            vec![
                BoardingPointId::new("other-metro-stop"),
                BoardingPointId::new("metro-end"),
            ],
        ));
        stops.push(stop_at(
            "metro-end",
            GeoCoordinate {
                latitude: 25.1,
                longitude: 121.5,
            },
        ));
        let network = TransitNetwork::fixture(
            time::Time::from_hms(4, 0, 0).unwrap(),
            stops,
            patterns,
            Vec::new(),
        );
        let transit = TransitRuntime::new(std::sync::Arc::new(network));
        let limits = JourneyPlanningLimits::default();
        let nearby = nearby_stops(
            &transit,
            Coordinate::try_new(origin.latitude, origin.longitude).unwrap(),
            &[
                TransitMode::Bus,
                TransitMode::Metro,
                TransitMode::Rail,
                TransitMode::HighSpeedRail,
            ],
            24,
            &limits,
        );

        assert_eq!(nearby.len(), 24);
        assert!(
            nearby
                .iter()
                .any(|(stop_id, _)| stop_id.as_str() == "metro-stop")
        );
        assert!(
            nearby
                .iter()
                .any(|(stop_id, _)| stop_id.as_str() == "other-metro-stop")
        );
    }

    #[test]
    fn nearby_stops_filters_access_and_egress_candidates_by_mode_before_selection() {
        let origin = GeoCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        };
        let far_away = GeoCoordinate {
            latitude: 25.1,
            longitude: 121.5,
        };
        let network = TransitNetwork::fixture(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![
                stop_at("bus", origin),
                stop_at("metro", origin),
                stop_at("bus-end", far_away),
                stop_at("metro-end", far_away),
            ],
            vec![
                TransitPattern::fixture(
                    crate::domain::ids::TransitPatternId::new("bus"),
                    TransitMode::Bus,
                    "bus",
                    "bus",
                    vec![BoardingPointId::new("bus"), BoardingPointId::new("bus-end")],
                ),
                TransitPattern::fixture(
                    crate::domain::ids::TransitPatternId::new("metro"),
                    TransitMode::Metro,
                    "metro",
                    "metro",
                    vec![
                        BoardingPointId::new("metro"),
                        BoardingPointId::new("metro-end"),
                    ],
                ),
            ],
            Vec::new(),
        );
        let transit = TransitRuntime::new(std::sync::Arc::new(network));
        let limits = JourneyPlanningLimits::default();
        let coordinate = Coordinate::try_new(origin.latitude, origin.longitude).unwrap();

        let metro_only = nearby_stops(&transit, coordinate, &[TransitMode::Metro], 8, &limits);
        assert_eq!(
            metro_only
                .iter()
                .map(|(stop_id, _)| stop_id.as_str())
                .collect::<Vec<_>>(),
            vec!["metro"]
        );

        let bus_and_metro = nearby_stops(
            &transit,
            coordinate,
            &[TransitMode::Bus, TransitMode::Metro],
            8,
            &limits,
        );
        assert_eq!(
            bus_and_metro
                .iter()
                .map(|(stop_id, _)| stop_id.as_str())
                .collect::<Vec<_>>(),
            vec!["bus", "metro"]
        );
    }
}
