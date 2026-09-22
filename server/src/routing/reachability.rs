//! Destination-side structural reachability.
//!
//! This module intentionally imports no timetable or realtime types. It
//! answers only whether a service position can structurally reach an egress
//! within the transit-leg budget.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::domain::{
    ids::{BoardingPointId, TransitServiceId},
    transit::{TransitMode, TransitNetwork},
};

use super::{
    model::EgressPoint,
    topology::{ServicePosition, TransitTopologyIndex},
    transfers::TransferShortcutIndex,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachableBoarding {
    pub service_id: TransitServiceId,
    pub board_index: usize,
    pub min_remaining_legs: usize,
}

#[derive(Clone, Debug, Default)]
pub struct DestinationReachability {
    boardings_by_stop: HashMap<BoardingPointId, Vec<ReachableBoarding>>,
    minimums_by_position: HashMap<ServicePosition, usize>,
}

impl DestinationReachability {
    pub fn build(
        network: &TransitNetwork,
        topology: &TransitTopologyIndex,
        transfers: &TransferShortcutIndex,
        destination_egress: &[EgressPoint],
        allowed_transit_modes: &[TransitMode],
        max_legs: usize,
    ) -> Self {
        let started_at = Instant::now();
        let mut minimums = HashMap::<ServicePosition, usize>::new();
        let mut targets = destination_egress
            .iter()
            .map(|egress| egress.stop_id.clone())
            .collect::<HashSet<_>>();

        for remaining_legs in 1..=max_legs {
            let mut newly_reachable = Vec::<ServicePosition>::new();
            for target in &targets {
                for alight in topology.occurrences_at(target) {
                    let Some(service) = network.service(&alight.service_id) else {
                        continue;
                    };
                    if !allowed_transit_modes.contains(&service.transit_mode) {
                        continue;
                    }
                    for board_index in 0..alight.stop_index {
                        let position = ServicePosition {
                            service_id: service.id.clone(),
                            stop_index: board_index,
                        };
                        if minimums
                            .get(&position)
                            .is_none_or(|known| remaining_legs < *known)
                        {
                            minimums.insert(position.clone(), remaining_legs);
                            newly_reachable.push(position);
                        }
                    }
                }
            }

            if remaining_legs == max_legs || newly_reachable.is_empty() {
                break;
            }
            targets.clear();
            for position in newly_reachable {
                let Some(service) = network.service(&position.service_id) else {
                    continue;
                };
                let Some(board_stop) = service.pattern.get(position.stop_index) else {
                    continue;
                };
                targets.insert(board_stop.clone());
                targets.extend(
                    transfers
                        .transfers_to(board_stop)
                        .iter()
                        .map(|shortcut| shortcut.from.clone()),
                );
            }
        }

        let mut boardings_by_stop = HashMap::<BoardingPointId, Vec<ReachableBoarding>>::new();
        for (position, min_remaining_legs) in &minimums {
            let Some(service) = network.service(&position.service_id) else {
                continue;
            };
            let Some(stop_id) = service.pattern.get(position.stop_index) else {
                continue;
            };
            boardings_by_stop
                .entry(stop_id.clone())
                .or_default()
                .push(ReachableBoarding {
                    service_id: position.service_id.clone(),
                    board_index: position.stop_index,
                    min_remaining_legs: *min_remaining_legs,
                });
        }
        for boardings in boardings_by_stop.values_mut() {
            boardings.sort_by(|left, right| {
                left.service_id
                    .as_str()
                    .cmp(right.service_id.as_str())
                    .then(left.board_index.cmp(&right.board_index))
            });
        }
        let result = Self {
            boardings_by_stop,
            minimums_by_position: minimums,
        };
        tracing::debug!(
            target: "via_server::routing",
            egress_stops = destination_egress.len(),
            reachable_boarding_opportunities = result.len(),
            reachable_stops = result.boardings_by_stop.len(),
            reachable_services = result.service_count(),
            max_legs,
            latency_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "destination reachability"
        );
        result
    }

    pub fn boardings_at(&self, stop: &BoardingPointId) -> &[ReachableBoarding] {
        self.boardings_by_stop
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn remaining_legs(
        &self,
        service_id: &TransitServiceId,
        board_index: usize,
    ) -> Option<usize> {
        self.minimums_by_position
            .get(&ServicePosition {
                service_id: service_id.clone(),
                stop_index: board_index,
            })
            .copied()
    }

    pub fn positions(&self) -> impl Iterator<Item = ServicePosition> + '_ {
        self.boardings_by_stop
            .values()
            .flatten()
            .map(|boarding| ServicePosition {
                service_id: boarding.service_id.clone(),
                stop_index: boarding.board_index,
            })
    }

    pub fn bus_stops(&self, network: &TransitNetwork) -> Vec<BoardingPointId> {
        let mut stops = self
            .boardings_by_stop
            .iter()
            .filter(|(_, boardings)| {
                boardings.iter().any(|boarding| {
                    network
                        .service(&boarding.service_id)
                        .is_some_and(|service| service.transit_mode == TransitMode::Bus)
                })
            })
            .map(|(stop, _)| stop.clone())
            .collect::<Vec<_>>();
        stops.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        stops
    }

    pub fn len(&self) -> usize {
        self.boardings_by_stop.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.boardings_by_stop.is_empty()
    }

    pub fn service_count(&self) -> usize {
        self.boardings_by_stop
            .values()
            .flatten()
            .map(|boarding| &boarding.service_id)
            .collect::<HashSet<_>>()
            .len()
    }
}

#[cfg(test)]
mod tests {
    use time::Time;

    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::transit::{TransferEdge, TransitService, TransitStop},
        routing::transfers::TransferShortcutIndex,
    };

    fn id(value: &str) -> BoardingPointId {
        BoardingPointId::new(value)
    }

    fn network(
        services: &[(&str, TransitMode, &[&str])],
        transfers: &[(&str, &str)],
    ) -> TransitNetwork {
        let stop_ids = services
            .iter()
            .flat_map(|(_, _, pattern)| pattern.iter().copied())
            .collect::<HashSet<_>>();
        TransitNetwork::new(
            Time::from_hms(4, 0, 0).unwrap(),
            stop_ids
                .into_iter()
                .map(|stop_id| {
                    TransitStop::new(
                        id(stop_id),
                        stop_id,
                        RequestCoordinate {
                            latitude: 25.0,
                            longitude: 121.5,
                        },
                    )
                })
                .collect(),
            services
                .iter()
                .map(|(service_id, mode, pattern)| {
                    TransitService::new(
                        TransitServiceId::new(*service_id),
                        *mode,
                        *service_id,
                        pattern.last().copied().unwrap_or_default(),
                        pattern.iter().map(|stop_id| id(stop_id)).collect(),
                    )
                })
                .collect(),
            Vec::new(),
        )
        .with_transfer_edges(
            transfers
                .iter()
                .map(|(from, to)| TransferEdge::new(id(from), id(to), 60))
                .collect(),
        )
    }

    fn build(
        network: &TransitNetwork,
        destination: &str,
        modes: &[TransitMode],
        max_legs: usize,
    ) -> DestinationReachability {
        DestinationReachability::build(
            network,
            &TransitTopologyIndex::new(network),
            &TransferShortcutIndex::from_network(network),
            &[EgressPoint::new(id(destination), 0, 0)],
            modes,
            max_legs,
        )
    }

    #[test]
    fn direct_route_respects_direction_and_excludes_destination_boarding() {
        let network = network(&[("line", TransitMode::Bus, &["a", "b", "c"])], &[]);
        let forward = build(&network, "c", &[TransitMode::Bus], 1);
        assert_eq!(
            forward.remaining_legs(&TransitServiceId::new("line"), 0),
            Some(1)
        );
        assert_eq!(
            forward.remaining_legs(&TransitServiceId::new("line"), 1),
            Some(1)
        );
        assert_eq!(
            forward.remaining_legs(&TransitServiceId::new("line"), 2),
            None
        );

        let reverse = build(&network, "a", &[TransitMode::Bus], 1);
        assert!(reverse.is_empty());
    }

    #[test]
    fn transfer_direction_mode_filter_and_leg_limit_are_structural() {
        let transit_network = network(
            &[
                ("bus", TransitMode::Bus, &["a", "b"]),
                ("metro", TransitMode::Metro, &["c", "d"]),
            ],
            &[("b", "c")],
        );
        let both = build(
            &transit_network,
            "d",
            &[TransitMode::Bus, TransitMode::Metro],
            2,
        );
        assert_eq!(
            both.remaining_legs(&TransitServiceId::new("metro"), 0),
            Some(1)
        );
        assert_eq!(
            both.remaining_legs(&TransitServiceId::new("bus"), 0),
            Some(2)
        );

        let one_leg = build(
            &transit_network,
            "d",
            &[TransitMode::Bus, TransitMode::Metro],
            1,
        );
        assert_eq!(
            one_leg.remaining_legs(&TransitServiceId::new("bus"), 0),
            None
        );
        let metro_only = build(&transit_network, "d", &[TransitMode::Metro], 2);
        assert_eq!(
            metro_only.remaining_legs(&TransitServiceId::new("bus"), 0),
            None
        );

        let reversed = network(
            &[
                ("bus", TransitMode::Bus, &["a", "b"]),
                ("metro", TransitMode::Metro, &["c", "d"]),
            ],
            &[("c", "b")],
        );
        assert_eq!(
            build(&reversed, "d", &[TransitMode::Bus, TransitMode::Metro], 2)
                .remaining_legs(&TransitServiceId::new("bus"), 0),
            None
        );
    }

    #[test]
    fn loop_occurrences_are_distinguished_by_stop_index() {
        let network = network(&[("loop", TransitMode::Bus, &["a", "b", "a", "d"])], &[]);
        let reachability = build(&network, "d", &[TransitMode::Bus], 1);
        assert_eq!(
            reachability.remaining_legs(&TransitServiceId::new("loop"), 0),
            Some(1)
        );
        assert_eq!(
            reachability.remaining_legs(&TransitServiceId::new("loop"), 2),
            Some(1)
        );
        assert_eq!(reachability.boardings_at(&id("a")).len(), 2);
    }

    #[test]
    fn reachability_does_not_read_or_require_a_timetable() {
        let network = network(&[("late", TransitMode::Bus, &["a", "b"])], &[]);
        assert!(network.trips.is_empty());
        assert_eq!(
            build(&network, "b", &[TransitMode::Bus], 1)
                .remaining_legs(&TransitServiceId::new("late"), 0),
            Some(1)
        );
    }
}
