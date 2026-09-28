use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use time::OffsetDateTime;

use crate::{
    domain::bikeshare::{
        BikeAvailabilitySnapshot, BikeShareNetwork, BikeStationAccess, BikeWalkingAccess,
        DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
    },
    domain::ids::BikeStationId,
    providers::BikeAvailabilitySource,
    street::{
        Coordinate, MatrixCell, MatrixQuery, RoutingConstraints, StreetRouter, StreetTravelMode,
    },
};

use super::path::{BikeRide, BikeShortcut};
use super::static_search::{CandidateCorridor, StaticCorridorStep};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BikePlanningStats {
    pub relevant_bike_stations: usize,
    pub candidate_movements: usize,
    pub pickup_stations_considered: usize,
    pub dropoff_stations_considered: usize,
    pub station_pairs: usize,
    pub matrix_requests: usize,
    pub realizations_feasible: usize,
}

pub(crate) const MAX_PICKUP_STATIONS_PER_ANCHOR: usize = 3;
pub(crate) const MAX_DROPOFF_STATIONS_PER_ANCHOR: usize = 3;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct CorridorMovement {
    pub from: crate::domain::ids::BoardingPointId,
    pub to: crate::domain::ids::BoardingPointId,
}

impl CorridorMovement {
    fn new(
        from: crate::domain::ids::BoardingPointId,
        to: crate::domain::ids::BoardingPointId,
    ) -> Self {
        Self { from, to }
    }
}

pub(crate) type BikeMovementRealizations = HashMap<CorridorMovement, Vec<BikeShortcut>>;

#[derive(Clone)]
pub(crate) struct BikeSegmentPlanner {
    pub(crate) network: Arc<BikeShareNetwork>,
    pub(crate) availability_source: Arc<dyn BikeAvailabilitySource>,
    pub(crate) street_router: Arc<dyn StreetRouter>,
}

impl BikeSegmentPlanner {
    pub(crate) fn new(
        network: Arc<BikeShareNetwork>,
        availability_source: Arc<dyn BikeAvailabilitySource>,
        street_router: Arc<dyn StreetRouter>,
    ) -> Self {
        Self {
            network,
            availability_source,
            street_router,
        }
    }

    pub(crate) async fn movement_realizations(
        &self,
        corridor: &CandidateCorridor,
        snapshot: &BikeAvailabilitySnapshot,
        now: OffsetDateTime,
    ) -> (BikeMovementRealizations, BikePlanningStats) {
        if corridor.is_empty() {
            return (HashMap::new(), BikePlanningStats::default());
        }

        let view =
            self.network
                .availability_view_at(snapshot, now, DEFAULT_BIKE_AVAILABILITY_MAX_AGE);
        let mut pair_needs = HashMap::<StationPair, Vec<PairNeed>>::new();
        let mut relevant_stations = HashSet::new();
        let mut movements = HashSet::new();
        for retained in corridor.retained() {
            for step in &retained.steps {
                let movement = match step {
                    StaticCorridorStep::Transit(_) => continue,
                    StaticCorridorStep::SurfaceTransfer { from, to, .. } => {
                        CorridorMovement::new(from.clone(), to.clone())
                    }
                };
                movements.insert(movement);
            }
        }

        let mut pickup_stations_considered = 0;
        let mut dropoff_stations_considered = 0;

        for movement in movements {
            let pickups = station_choices(
                &self.network,
                view.usable_pickups(&movement.from),
                true,
                &mut relevant_stations,
            );
            let dropoffs = station_choices(
                &self.network,
                view.usable_dropoffs(&movement.to),
                false,
                &mut relevant_stations,
            );
            pickup_stations_considered += pickups.len();
            dropoff_stations_considered += dropoffs.len();
            if pickups.is_empty() || dropoffs.is_empty() {
                continue;
            }
            for pickup in &pickups {
                for dropoff in &dropoffs {
                    if pickup.station_id == dropoff.station_id {
                        continue;
                    }
                    pair_needs
                        .entry(StationPair {
                            pickup_station_id: pickup.station_id.clone(),
                            dropoff_station_id: dropoff.station_id.clone(),
                        })
                        .or_default()
                        .push(PairNeed {
                            movement: movement.clone(),
                            walk_to_pickup: pickup.walking,
                            walk_from_dropoff: dropoff.walking,
                        });
                }
            }
        }

        let mut stats = BikePlanningStats {
            relevant_bike_stations: relevant_stations.len(),
            candidate_movements: pair_needs
                .values()
                .flat_map(|needs| needs.iter().map(|need| need.movement.clone()))
                .collect::<HashSet<_>>()
                .len(),
            pickup_stations_considered,
            dropoff_stations_considered,
            station_pairs: pair_needs.len(),
            ..BikePlanningStats::default()
        };
        let realizations = self.matrix_realizations(&pair_needs, &mut stats).await;
        stats.realizations_feasible = realizations.values().map(Vec::len).sum();
        (realizations, stats)
    }

    async fn matrix_realizations(
        &self,
        pair_needs: &HashMap<StationPair, Vec<PairNeed>>,
        stats: &mut BikePlanningStats,
    ) -> BikeMovementRealizations {
        if pair_needs.is_empty() {
            return HashMap::new();
        }

        let mut targets_by_pickup = HashMap::<BikeStationId, Vec<BikeStationId>>::new();
        for pair in pair_needs.keys() {
            let targets = targets_by_pickup
                .entry(pair.pickup_station_id.clone())
                .or_default();
            if !targets.contains(&pair.dropoff_station_id) {
                targets.push(pair.dropoff_station_id.clone());
            }
        }
        let mut grouped = HashMap::<Vec<String>, (Vec<BikeStationId>, Vec<BikeStationId>)>::new();
        for (pickup, targets) in targets_by_pickup {
            let mut targets = targets;
            targets.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            let key = targets
                .iter()
                .map(|target| target.as_str().to_owned())
                .collect::<Vec<_>>();
            grouped
                .entry(key)
                .or_insert_with(|| (targets, Vec::new()))
                .1
                .push(pickup);
        }

        let limits = self.street_router.matrix_limits();
        if limits.max_sources == 0 || limits.max_targets == 0 {
            return HashMap::new();
        }
        let mut accepted = HashMap::<CorridorMovement, Vec<BikeShortcut>>::new();
        let mut grouped = grouped.into_values().collect::<Vec<_>>();
        grouped.sort_by(|(left, _), (right, _)| {
            left.iter()
                .map(BikeStationId::as_str)
                .cmp(right.iter().map(BikeStationId::as_str))
        });
        for (target_ids, mut pickup_ids) in grouped {
            pickup_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            for pickup_chunk in pickup_ids.chunks(limits.max_sources) {
                for target_chunk in target_ids.chunks(limits.max_targets) {
                    let Some(sources) = pickup_chunk
                        .iter()
                        .map(|id| station_coordinate(&self.network, id))
                        .collect::<Option<Vec<_>>>()
                    else {
                        continue;
                    };
                    let Some(targets) = target_chunk
                        .iter()
                        .map(|id| station_coordinate(&self.network, id))
                        .collect::<Option<Vec<_>>>()
                    else {
                        continue;
                    };
                    stats.matrix_requests += 1;
                    let Ok(result) = self
                        .street_router
                        .matrix(MatrixQuery {
                            orientation: crate::street::MatrixOrientation::ManyToMany,
                            travel_mode: StreetTravelMode::Bicycle,
                            sources,
                            targets,
                            constraints: RoutingConstraints::default(),
                        })
                        .await
                    else {
                        continue;
                    };
                    if result.cells.len() != pickup_chunk.len()
                        || result
                            .cells
                            .iter()
                            .any(|row| row.len() != target_chunk.len())
                    {
                        continue;
                    }
                    for (source_index, row) in result.cells.iter().enumerate() {
                        for (target_index, cell) in row.iter().enumerate() {
                            let MatrixCell::Reachable { duration, distance } = cell else {
                                continue;
                            };
                            let pair = StationPair {
                                pickup_station_id: pickup_chunk[source_index].clone(),
                                dropoff_station_id: target_chunk[target_index].clone(),
                            };
                            let Some(needs) = pair_needs.get(&pair) else {
                                continue;
                            };
                            for need in needs {
                                let Some(shortcut) = BikeShortcut::new(
                                    0,
                                    1,
                                    pair.pickup_station_id.clone(),
                                    pair.dropoff_station_id.clone(),
                                    need.walk_to_pickup,
                                    BikeRide {
                                        duration_seconds: duration.0,
                                        distance_metres: distance.0,
                                    },
                                    need.walk_from_dropoff,
                                ) else {
                                    continue;
                                };
                                accepted
                                    .entry(need.movement.clone())
                                    .or_default()
                                    .push(shortcut);
                            }
                        }
                    }
                }
            }
        }
        for shortcuts in accepted.values_mut() {
            shortcuts.sort_by(|left, right| {
                (
                    left.total_seconds,
                    left.pickup_station_id.as_str(),
                    left.dropoff_station_id.as_str(),
                )
                    .cmp(&(
                        right.total_seconds,
                        right.pickup_station_id.as_str(),
                        right.dropoff_station_id.as_str(),
                    ))
            });
            shortcuts.dedup();
        }
        accepted
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct StationPair {
    pickup_station_id: BikeStationId,
    dropoff_station_id: BikeStationId,
}

struct PairNeed {
    movement: CorridorMovement,
    walk_to_pickup: BikeWalkingAccess,
    walk_from_dropoff: BikeWalkingAccess,
}

#[derive(Clone)]
struct StationChoice {
    station_id: BikeStationId,
    walking: BikeWalkingAccess,
}

fn station_choices<'a>(
    network: &BikeShareNetwork,
    accesses: impl IntoIterator<Item = &'a BikeStationAccess>,
    pickup: bool,
    relevant_stations: &mut HashSet<BikeStationId>,
) -> Vec<StationChoice> {
    let mut choices = HashMap::<BikeStationId, BikeWalkingAccess>::new();
    for access in accesses {
        let Some(walking) = (if pickup {
            access.walk_to_station
        } else {
            access.walk_from_station
        }) else {
            continue;
        };
        if network.station(&access.station_id).is_none() {
            continue;
        }
        choices
            .entry(access.station_id.clone())
            .and_modify(|current| {
                if walking.duration_seconds < current.duration_seconds {
                    *current = walking;
                }
            })
            .or_insert(walking);
    }
    let mut choices = choices
        .into_iter()
        .map(|(station_id, walking)| StationChoice {
            station_id,
            walking,
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.walking
            .duration_seconds
            .cmp(&right.walking.duration_seconds)
            .then(
                left.walking
                    .distance_metres
                    .cmp(&right.walking.distance_metres),
            )
            .then(left.station_id.as_str().cmp(right.station_id.as_str()))
    });
    choices.truncate(if pickup {
        MAX_PICKUP_STATIONS_PER_ANCHOR
    } else {
        MAX_DROPOFF_STATIONS_PER_ANCHOR
    });
    relevant_stations.extend(choices.iter().map(|choice| choice.station_id.clone()));
    choices
}

fn station_coordinate(network: &BikeShareNetwork, id: &BikeStationId) -> Option<Coordinate> {
    let station = network.station(id)?;
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude).ok()
}

#[cfg(test)]
mod tests {
    use time::{Time, macros::datetime};

    use crate::{
        api::RequestCoordinate,
        domain::{
            bikeshare::{
                BikeAvailabilitySnapshot, BikeShareNetwork, BikeStation, BikeStationAccess,
                BikeStationAvailability,
            },
            ids::BoardingPointId,
            transit::TransitNetwork,
            transit_runtime::TransitRuntime,
        },
        providers::UnavailableBikeAvailabilitySource,
        routing::{StaticCorridor, StaticCostSource, static_search::StaticCorridorStep},
        street::FakeStreetRouter,
    };

    use super::*;

    #[test]
    fn station_choices_are_bounded_to_three_nearest_per_anchor() {
        let anchor = BoardingPointId::new("anchor");
        let stations = (0..5)
            .map(|index| {
                BikeStation::new(
                    BikeStationId::new(format!("station-{index}")),
                    format!("Station {index}"),
                    RequestCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                )
            })
            .collect::<Vec<_>>();
        let access_links = (0..5)
            .map(|index| {
                BikeStationAccess::new(
                    anchor.clone(),
                    BikeStationId::new(format!("station-{index}")),
                    Some(BikeWalkingAccess::new(index + 1, index + 1)),
                    Some(BikeWalkingAccess::new(index + 1, index + 1)),
                )
            })
            .collect();
        let network = BikeShareNetwork::new(stations).with_access_links(access_links);
        let mut relevant = HashSet::new();

        let pickups = station_choices(
            &network,
            network.access_from_boarding_point(&anchor),
            true,
            &mut relevant,
        );
        let dropoffs = station_choices(
            &network,
            network.access_to_boarding_point(&anchor),
            false,
            &mut relevant,
        );

        assert_eq!(pickups.len(), MAX_PICKUP_STATIONS_PER_ANCHOR);
        assert_eq!(dropoffs.len(), MAX_DROPOFF_STATIONS_PER_ANCHOR);
        assert_eq!(relevant.len(), 3);
        assert_eq!(
            pickups
                .iter()
                .map(|choice| choice.station_id.as_str())
                .collect::<Vec<_>>(),
            vec!["station-0", "station-1", "station-2"]
        );
    }

    #[tokio::test]
    async fn cached_bike_availability_is_checked_at_the_request_time() {
        let from = BoardingPointId::new("from");
        let to = BoardingPointId::new("to");
        let pickup_id = BikeStationId::new("tdx:Taipei:pickup");
        let dropoff_id = BikeStationId::new("tdx:Taipei:dropoff");
        let bike_network = Arc::new(
            BikeShareNetwork::new(vec![
                BikeStation::new(
                    pickup_id.clone(),
                    "Pickup",
                    RequestCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                ),
                BikeStation::new(
                    dropoff_id.clone(),
                    "Dropoff",
                    RequestCoordinate {
                        latitude: 25.001,
                        longitude: 121.501,
                    },
                ),
            ])
            .with_access_links(vec![
                BikeStationAccess::new(
                    from.clone(),
                    pickup_id.clone(),
                    Some(BikeWalkingAccess::new(10, 50)),
                    None,
                ),
                BikeStationAccess::new(
                    to.clone(),
                    dropoff_id.clone(),
                    None,
                    Some(BikeWalkingAccess::new(10, 50)),
                ),
            ])
            .with_revision("fixture"),
        );
        let transit = TransitRuntime::new(Arc::new(TransitNetwork::fixture(
            Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )));
        let corridor = CandidateCorridor::from_retained_for_test(
            &transit,
            vec![StaticCorridor {
                access_stop_id: from.clone(),
                egress_stop_id: to.clone(),
                theoretical_duration_seconds: 0,
                cost_source: StaticCostSource::OptimisticEstimate,
                steps: vec![StaticCorridorStep::SurfaceTransfer {
                    from,
                    to,
                    walking_duration_seconds: 60,
                }],
            }],
        );
        let received_at = datetime!(2026-09-21 10:00 UTC);
        let mut stations = HashMap::new();
        for (station_id, bikes, docks) in [(pickup_id, 1, 0), (dropoff_id, 0, 1)] {
            stations.insert(
                station_id.clone(),
                BikeStationAvailability::new(
                    station_id,
                    bikes,
                    docks,
                    true,
                    received_at - time::Duration::seconds(110),
                ),
            );
        }
        let snapshot = BikeAvailabilitySnapshot::new(received_at, stations);
        let planner = BikeSegmentPlanner::new(
            bike_network,
            Arc::new(UnavailableBikeAvailabilitySource),
            Arc::new(FakeStreetRouter::builder().build()),
        );

        let (_, stats) = planner
            .movement_realizations(
                &corridor,
                &snapshot,
                received_at + time::Duration::seconds(11),
            )
            .await;

        assert_eq!(stats.pickup_stations_considered, 0);
        assert_eq!(stats.dropoff_stations_considered, 0);
        assert_eq!(stats.station_pairs, 0);
    }
}
