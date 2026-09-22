//! BikeShare alternatives for adjacent movements in a transit path.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

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

use super::path::{BikeRide, BikeShortcut, JourneySkeleton, JourneyVariant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BikePlanningStats {
    pub relevant_bike_stations: usize,
    pub candidate_segments: usize,
    pub station_pairs: usize,
    pub matrix_requests: usize,
    pub shortcuts_accepted: usize,
    pub final_variants: usize,
}

/// The planner-owned BikeShare dependencies and the small amount of work used
/// after temporal transit search has produced candidate paths.
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

    pub(crate) async fn choose_variants(
        &self,
        skeletons: Vec<JourneySkeleton>,
        snapshot: &BikeAvailabilitySnapshot,
        max_variants: usize,
    ) -> (Vec<JourneyVariant>, BikePlanningStats) {
        if skeletons.is_empty() {
            return (Vec::new(), BikePlanningStats::default());
        }

        let view = self.network.availability_view_at(
            snapshot,
            snapshot.received_at,
            DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
        );
        let mut pair_needs = HashMap::<StationPair, Vec<PairNeed>>::new();
        let mut relevant_stations = HashSet::new();
        let mut candidate_segments = 0;

        for (skeleton_index, skeleton) in skeletons.iter().enumerate() {
            for (movement_index, movement) in skeleton.movements().iter().enumerate() {
                let (Some(from_id), Some(to_id)) = (
                    boarding_point(skeleton, movement.from_anchor),
                    boarding_point(skeleton, movement.to_anchor),
                ) else {
                    continue;
                };
                let baseline_seconds =
                    u32::try_from((movement.arrival_at - movement.departure_at).whole_seconds())
                        .unwrap_or(0);
                if baseline_seconds == 0 {
                    continue;
                }

                let pickups = station_choices(
                    &self.network,
                    view.usable_pickups(&from_id),
                    true,
                    &mut relevant_stations,
                );
                let dropoffs = station_choices(
                    &self.network,
                    view.usable_dropoffs(&to_id),
                    false,
                    &mut relevant_stations,
                );
                if pickups.is_empty() || dropoffs.is_empty() {
                    continue;
                }
                candidate_segments += 1;
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
                                skeleton_index,
                                movement_index,
                                from_anchor: movement.from_anchor,
                                to_anchor: movement.to_anchor,
                                baseline_seconds,
                                walk_to_pickup: pickup.walking,
                                walk_from_dropoff: dropoff.walking,
                            });
                    }
                }
            }
        }

        let mut stats = BikePlanningStats {
            relevant_bike_stations: relevant_stations.len(),
            candidate_segments,
            station_pairs: pair_needs.len(),
            ..BikePlanningStats::default()
        };
        let shortcuts = self.matrix_shortcuts(&pair_needs, &mut stats).await;
        stats.shortcuts_accepted = shortcuts.values().map(Vec::len).sum();

        let mut variants = Vec::new();
        for (skeleton_index, skeleton) in skeletons.into_iter().enumerate() {
            let segment_shortcuts = shortcuts
                .iter()
                .filter_map(|((index, movement), shortcuts)| {
                    (*index == skeleton_index).then_some((*movement, shortcuts.as_slice()))
                })
                .collect::<Vec<_>>();
            let selected = build_variants(skeleton, segment_shortcuts, max_variants.max(1));
            stats.final_variants += selected.len();
            variants.extend(selected);
        }
        (variants, stats)
    }

    async fn matrix_shortcuts(
        &self,
        pair_needs: &HashMap<StationPair, Vec<PairNeed>>,
        stats: &mut BikePlanningStats,
    ) -> HashMap<(usize, usize), Vec<BikeShortcut>> {
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
        let mut accepted = HashMap::<(usize, usize), Vec<BikeShortcut>>::new();
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
                                    need.from_anchor,
                                    need.to_anchor,
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
                                if shortcut.total_seconds >= need.baseline_seconds {
                                    continue;
                                }
                                accepted
                                    .entry((need.skeleton_index, need.movement_index))
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
    skeleton_index: usize,
    movement_index: usize,
    from_anchor: usize,
    to_anchor: usize,
    baseline_seconds: u32,
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
        relevant_stations.insert(access.station_id.clone());
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
    choices.sort_by(|left, right| left.station_id.as_str().cmp(right.station_id.as_str()));
    choices
}

fn boarding_point(
    skeleton: &JourneySkeleton,
    anchor: usize,
) -> Option<crate::domain::ids::BoardingPointId> {
    match &skeleton.anchors().get(anchor)?.kind {
        super::path::JourneyAnchorKind::BoardingPoint { id } => Some(id.clone()),
        super::path::JourneyAnchorKind::Origin | super::path::JourneyAnchorKind::Destination => {
            None
        }
    }
}

fn station_coordinate(network: &BikeShareNetwork, id: &BikeStationId) -> Option<Coordinate> {
    let station = network.station(id)?;
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude).ok()
}

fn build_variants(
    skeleton: JourneySkeleton,
    segment_shortcuts: Vec<(usize, &[BikeShortcut])>,
    max_variants: usize,
) -> Vec<JourneyVariant> {
    let baseline = JourneyVariant::baseline(skeleton);
    let mut variants = vec![baseline.clone()];
    for (movement_index, shortcuts) in segment_shortcuts {
        let previous = variants.clone();
        let mut next = vec![baseline.clone()];
        for variant in previous {
            for shortcut in shortcuts {
                if let Some(candidate) =
                    JourneyVariant::with_replacement(&variant, movement_index, shortcut.clone())
                {
                    next.push(candidate);
                }
            }
        }
        next.sort_by(variant_order);
        next.dedup_by(|left, right| left.signature() == right.signature());
        next.truncate(max_variants);
        variants = next;
    }
    variants
}

fn variant_order(left: &JourneyVariant, right: &JourneyVariant) -> std::cmp::Ordering {
    let left_timing = left.evaluate_timing().ok();
    let right_timing = right.evaluate_timing().ok();
    left.is_baseline()
        .cmp(&right.is_baseline())
        .reverse()
        .then_with(|| {
            left_timing
                .as_ref()
                .map(|timing| timing.arrival_at)
                .cmp(&right_timing.as_ref().map(|timing| timing.arrival_at))
        })
        .then_with(|| {
            left_timing
                .as_ref()
                .map(|timing| timing.bike_share_uses)
                .cmp(&right_timing.as_ref().map(|timing| timing.bike_share_uses))
        })
        .then_with(|| left.signature().cmp(&right.signature()))
}
