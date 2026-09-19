use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use time::{Duration, OffsetDateTime};

use crate::{
    api::RequestCoordinate,
    domain::{
        bikeshare::{
            BikeShareNetwork, BikeStationAccess, BikeWalkingAccess,
            DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
        },
        ids::BikeStationId,
    },
    providers::BikeAvailabilitySource,
    street::{
        Coordinate, MatrixCell, MatrixOrientation, MatrixQuery, Metres, RoutingConstraints,
        StreetRouter, StreetTravelMode,
    },
};

use super::{
    BikeRide, BikeShortcut, JourneyAnchorKind, JourneyAugmenter, JourneyPathMovement,
    JourneyPathStep, JourneySkeleton, JourneyVariant,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeAugmentationPolicy {
    pub max_variants: usize,
    pub availability_max_age: Duration,
    pub max_endpoint_geometric_radius_metres: u32,
    pub max_endpoint_station_candidates: usize,
    pub max_endpoint_walking_seconds: u32,
    pub max_endpoint_walking_distance_metres: u32,
    pub now: Option<OffsetDateTime>,
}

impl Default for BikeAugmentationPolicy {
    fn default() -> Self {
        Self {
            max_variants: 8,
            availability_max_age: DEFAULT_BIKE_AVAILABILITY_MAX_AGE,
            max_endpoint_geometric_radius_metres: 300,
            max_endpoint_station_candidates: 16,
            max_endpoint_walking_seconds: 8 * 60,
            max_endpoint_walking_distance_metres: 800,
            now: None,
        }
    }
}

#[derive(Clone)]
pub struct BikeJourneyAugmenter {
    network: Arc<BikeShareNetwork>,
    availability_source: Arc<dyn BikeAvailabilitySource>,
    street_router: Arc<dyn StreetRouter>,
    policy: BikeAugmentationPolicy,
}

impl BikeJourneyAugmenter {
    pub fn new(
        network: Arc<BikeShareNetwork>,
        availability_source: Arc<dyn BikeAvailabilitySource>,
        street_router: Arc<dyn StreetRouter>,
        policy: BikeAugmentationPolicy,
    ) -> Self {
        Self {
            network,
            availability_source,
            street_router,
            policy,
        }
    }

    pub fn with_default_policy(
        network: Arc<BikeShareNetwork>,
        availability_source: Arc<dyn BikeAvailabilitySource>,
        street_router: Arc<dyn StreetRouter>,
    ) -> Self {
        Self::new(
            network,
            availability_source,
            street_router,
            BikeAugmentationPolicy::default(),
        )
    }

    pub fn policy(&self) -> &BikeAugmentationPolicy {
        &self.policy
    }
}

#[async_trait]
impl JourneyAugmenter for BikeJourneyAugmenter {
    async fn augment(&self, skeleton: JourneySkeleton) -> Vec<JourneyVariant> {
        let baseline = JourneyVariant::baseline(skeleton.clone());
        let Ok(snapshot) = self.availability_source.snapshot().await else {
            return vec![baseline];
        };

        let now = self.policy.now.unwrap_or_else(OffsetDateTime::now_utc);
        let view =
            self.network
                .availability_view_at(&snapshot, now, self.policy.availability_max_age);
        let endpoint_choices = self.collect_endpoint_choices(&skeleton, &view).await;
        let Some(intervals) =
            collect_eligible_intervals(&skeleton, &self.network, &view, &endpoint_choices)
        else {
            return vec![baseline];
        };
        if intervals.is_empty() {
            return vec![baseline];
        }

        let Some(shortcuts) = self.matrix_shortcuts(&self.network, &intervals).await else {
            return vec![baseline];
        };
        if shortcuts.is_empty() {
            return vec![baseline];
        }

        build_variants(skeleton, shortcuts, self.policy.max_variants.max(1))
    }
}

impl BikeJourneyAugmenter {
    async fn collect_endpoint_choices(
        &self,
        skeleton: &JourneySkeleton,
        view: &crate::domain::bikeshare::BikeAvailabilityView<'_>,
    ) -> EndpointChoices {
        let Some(origin) = skeleton
            .anchors()
            .first()
            .and_then(|anchor| match &anchor.kind {
                JourneyAnchorKind::Origin { coordinate } => Some(*coordinate),
                JourneyAnchorKind::BoardingPoint { .. } | JourneyAnchorKind::Destination { .. } => {
                    None
                }
            })
        else {
            return EndpointChoices::default();
        };
        let Some(destination) = skeleton
            .anchors()
            .last()
            .and_then(|anchor| match &anchor.kind {
                JourneyAnchorKind::Destination { coordinate } => Some(*coordinate),
                JourneyAnchorKind::Origin { .. } | JourneyAnchorKind::BoardingPoint { .. } => None,
            })
        else {
            return EndpointChoices::default();
        };

        let pickup_candidates = endpoint_station_candidates(
            &self.network,
            origin,
            self.policy.max_endpoint_geometric_radius_metres,
            self.policy.max_endpoint_station_candidates,
            |station_id| view.station_usable_as_pickup(station_id),
        );
        let dropoff_candidates = endpoint_station_candidates(
            &self.network,
            destination,
            self.policy.max_endpoint_geometric_radius_metres,
            self.policy.max_endpoint_station_candidates,
            |station_id| view.station_usable_as_dropoff(station_id),
        );

        let pickups = self.origin_pickup_choices(origin, &pickup_candidates).await;
        let dropoffs = self
            .destination_dropoff_choices(destination, &dropoff_candidates)
            .await;
        EndpointChoices { pickups, dropoffs }
    }

    async fn origin_pickup_choices(
        &self,
        origin: RequestCoordinate,
        candidates: &[EndpointStationCandidate],
    ) -> Vec<StationChoice> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let limits = self.street_router.matrix_limits();
        let max_targets = limits.max_targets;
        if limits.max_sources == 0 || max_targets == 0 {
            return Vec::new();
        }
        let Ok(origin) = Coordinate::try_new(origin.latitude, origin.longitude) else {
            return Vec::new();
        };
        let constraints = endpoint_matrix_constraints(&self.policy);
        let mut choices = Vec::new();
        for chunk in candidates.chunks(max_targets) {
            let result = self
                .street_router
                .matrix(MatrixQuery {
                    orientation: MatrixOrientation::OneToMany,
                    travel_mode: StreetTravelMode::Walking,
                    sources: vec![origin],
                    targets: chunk.iter().map(|candidate| candidate.coordinate).collect(),
                    constraints,
                })
                .await
                .ok();
            let Some(result) = result else {
                return Vec::new();
            };
            let Some(row) = result.cells.first() else {
                return Vec::new();
            };
            if result.cells.len() != 1 || row.len() != chunk.len() {
                return Vec::new();
            }
            for (candidate, cell) in chunk.iter().zip(row) {
                if let Some(walking) = accepted_endpoint_metric(
                    *cell,
                    self.policy.max_endpoint_walking_seconds,
                    self.policy.max_endpoint_walking_distance_metres,
                ) {
                    choices.push(StationChoice {
                        station_id: candidate.station_id.clone(),
                        walking,
                    });
                }
            }
        }
        choices.sort_by(|left, right| left.station_id.as_str().cmp(right.station_id.as_str()));
        choices
    }

    async fn destination_dropoff_choices(
        &self,
        destination: RequestCoordinate,
        candidates: &[EndpointStationCandidate],
    ) -> Vec<StationChoice> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let limits = self.street_router.matrix_limits();
        let max_sources = limits.max_sources;
        if max_sources == 0 || limits.max_targets == 0 {
            return Vec::new();
        }
        let Ok(destination) = Coordinate::try_new(destination.latitude, destination.longitude)
        else {
            return Vec::new();
        };
        let constraints = endpoint_matrix_constraints(&self.policy);
        let mut choices = Vec::new();
        for chunk in candidates.chunks(max_sources) {
            let result = self
                .street_router
                .matrix(MatrixQuery {
                    orientation: MatrixOrientation::ManyToOne,
                    travel_mode: StreetTravelMode::Walking,
                    sources: chunk.iter().map(|candidate| candidate.coordinate).collect(),
                    targets: vec![destination],
                    constraints,
                })
                .await
                .ok();
            let Some(result) = result else {
                return Vec::new();
            };
            if result.cells.len() != chunk.len() || result.cells.iter().any(|row| row.len() != 1) {
                return Vec::new();
            }
            for (candidate, row) in chunk.iter().zip(result.cells) {
                let Some(cell) = row.into_iter().next() else {
                    return Vec::new();
                };
                if let Some(walking) = accepted_endpoint_metric(
                    cell,
                    self.policy.max_endpoint_walking_seconds,
                    self.policy.max_endpoint_walking_distance_metres,
                ) {
                    choices.push(StationChoice {
                        station_id: candidate.station_id.clone(),
                        walking,
                    });
                }
            }
        }
        choices.sort_by(|left, right| left.station_id.as_str().cmp(right.station_id.as_str()));
        choices
    }

    async fn matrix_shortcuts(
        &self,
        network: &BikeShareNetwork,
        intervals: &[EligibleInterval],
    ) -> Option<Vec<BikeShortcut>> {
        let mut pair_needs: HashMap<StationPair, Vec<PairNeed>> = HashMap::new();
        for (interval_index, interval) in intervals.iter().enumerate() {
            for pickup in &interval.pickups {
                for dropoff in &interval.dropoffs {
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
                            interval_index,
                            walk_to_pickup: pickup.walking,
                            walk_from_dropoff: dropoff.walking,
                        });
                }
            }
        }
        if pair_needs.is_empty() {
            return Some(Vec::new());
        }

        let mut target_ids_by_pickup: HashMap<BikeStationId, HashSet<BikeStationId>> =
            HashMap::new();
        for pair in pair_needs.keys() {
            target_ids_by_pickup
                .entry(pair.pickup_station_id.clone())
                .or_default()
                .insert(pair.dropoff_station_id.clone());
        }

        let mut grouped: HashMap<Vec<BikeStationId>, Vec<BikeStationId>> = HashMap::new();
        for (pickup_station_id, target_ids) in target_ids_by_pickup {
            let mut target_ids = target_ids.into_iter().collect::<Vec<_>>();
            target_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            grouped
                .entry(target_ids)
                .or_default()
                .push(pickup_station_id);
        }

        let mut grouped = grouped.into_iter().collect::<Vec<_>>();
        grouped.sort_by(|(left, _), (right, _)| {
            left.iter()
                .map(BikeStationId::as_str)
                .cmp(right.iter().map(BikeStationId::as_str))
        });

        let limits = self.street_router.matrix_limits();
        if limits.max_sources == 0 || limits.max_targets == 0 {
            return None;
        }
        let max_sources = limits.max_sources;
        let max_targets = limits.max_targets;
        let mut shortcuts = Vec::new();

        for (target_ids, mut pickup_ids) in grouped {
            pickup_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            for pickup_chunk in pickup_ids.chunks(max_sources) {
                for target_chunk in target_ids.chunks(max_targets) {
                    let sources = pickup_chunk
                        .iter()
                        .map(|id| station_coordinate(network, id))
                        .collect::<Option<Vec<_>>>()?;
                    let targets = target_chunk
                        .iter()
                        .map(|id| station_coordinate(network, id))
                        .collect::<Option<Vec<_>>>()?;
                    let result = self
                        .street_router
                        .matrix(MatrixQuery {
                            orientation: MatrixOrientation::ManyToMany,
                            travel_mode: StreetTravelMode::Bicycle,
                            sources,
                            targets,
                            constraints: RoutingConstraints::default(),
                        })
                        .await
                        .ok()?;
                    if result.cells.len() != pickup_chunk.len()
                        || result
                            .cells
                            .iter()
                            .any(|row| row.len() != target_chunk.len())
                    {
                        return None;
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
                            let needs = pair_needs.get(&pair)?;
                            for need in needs {
                                let interval = &intervals[need.interval_index];
                                let Some(shortcut) = BikeShortcut::new(
                                    interval.from_anchor,
                                    interval.to_anchor,
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
                                if shortcut.total_seconds < interval.baseline_seconds {
                                    shortcuts.push(shortcut);
                                }
                            }
                        }
                    }
                }
            }
        }

        shortcuts.sort_by(|left, right| {
            (
                left.from_anchor,
                left.to_anchor,
                left.pickup_station_id.as_str(),
                left.dropoff_station_id.as_str(),
                left.total_seconds,
            )
                .cmp(&(
                    right.from_anchor,
                    right.to_anchor,
                    right.pickup_station_id.as_str(),
                    right.dropoff_station_id.as_str(),
                    right.total_seconds,
                ))
        });
        shortcuts.dedup();
        Some(shortcuts)
    }
}

#[derive(Clone)]
struct StationChoice {
    station_id: BikeStationId,
    walking: BikeWalkingAccess,
}

#[derive(Clone, Default)]
struct EndpointChoices {
    pickups: Vec<StationChoice>,
    dropoffs: Vec<StationChoice>,
}

#[derive(Clone)]
struct EndpointStationCandidate {
    station_id: BikeStationId,
    coordinate: Coordinate,
    geometric_distance_metres: u64,
}

#[derive(Clone)]
struct EligibleInterval {
    from_anchor: usize,
    to_anchor: usize,
    baseline_seconds: u32,
    pickups: Vec<StationChoice>,
    dropoffs: Vec<StationChoice>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct StationPair {
    pickup_station_id: BikeStationId,
    dropoff_station_id: BikeStationId,
}

struct PairNeed {
    interval_index: usize,
    walk_to_pickup: BikeWalkingAccess,
    walk_from_dropoff: BikeWalkingAccess,
}

fn collect_eligible_intervals(
    skeleton: &JourneySkeleton,
    network: &BikeShareNetwork,
    view: &crate::domain::bikeshare::BikeAvailabilityView<'_>,
    endpoint_choices: &EndpointChoices,
) -> Option<Vec<EligibleInterval>> {
    let anchors = skeleton.anchors();
    let mut intervals = Vec::new();
    for from_anchor in 0..anchors.len() {
        for to_anchor in (from_anchor + 1)..anchors.len() {
            let pickups = match &anchors[from_anchor].kind {
                JourneyAnchorKind::Origin { .. } => endpoint_choices.pickups.clone(),
                JourneyAnchorKind::BoardingPoint { id } => {
                    station_choices(network, view.usable_pickups(id), true)
                }
                JourneyAnchorKind::Destination { .. } => continue,
            };
            let dropoffs = match &anchors[to_anchor].kind {
                JourneyAnchorKind::BoardingPoint { id } => {
                    station_choices(network, view.usable_dropoffs(id), false)
                }
                JourneyAnchorKind::Destination { .. } => endpoint_choices.dropoffs.clone(),
                JourneyAnchorKind::Origin { .. } => continue,
            };
            let elapsed = anchors[to_anchor].reached_at - anchors[from_anchor].reached_at;
            let baseline_seconds = u32::try_from(elapsed.whole_seconds()).ok()?;
            if baseline_seconds == 0 {
                continue;
            }
            if pickups.is_empty() || dropoffs.is_empty() {
                continue;
            }
            let minimum_walking = pickups
                .iter()
                .map(|choice| choice.walking.duration_seconds)
                .min()?
                .saturating_add(
                    dropoffs
                        .iter()
                        .map(|choice| choice.walking.duration_seconds)
                        .min()?,
                );
            if minimum_walking >= baseline_seconds {
                continue;
            }
            intervals.push(EligibleInterval {
                from_anchor,
                to_anchor,
                baseline_seconds,
                pickups,
                dropoffs,
            });
        }
    }
    Some(intervals)
}

fn station_choices(
    network: &BikeShareNetwork,
    accesses: Vec<&BikeStationAccess>,
    pickup: bool,
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
    choices.sort_by(|left, right| left.station_id.as_str().cmp(right.station_id.as_str()));
    choices
}

fn station_coordinate(network: &BikeShareNetwork, id: &BikeStationId) -> Option<Coordinate> {
    let station = network.station(id)?;
    Coordinate::try_new(station.coordinate.latitude, station.coordinate.longitude).ok()
}

fn endpoint_station_candidates(
    network: &BikeShareNetwork,
    endpoint: RequestCoordinate,
    max_radius_metres: u32,
    max_candidates: usize,
    usable: impl Fn(&BikeStationId) -> bool,
) -> Vec<EndpointStationCandidate> {
    if max_candidates == 0 {
        return Vec::new();
    }
    let mut candidates = network
        .stations
        .iter()
        .filter(|station| usable(&station.id))
        .filter_map(|station| {
            let coordinate = station_coordinate(network, &station.id)?;
            let geometric_distance_metres = endpoint.planar_distance_metres(station.coordinate);
            (geometric_distance_metres <= u64::from(max_radius_metres)).then_some(
                EndpointStationCandidate {
                    station_id: station.id.clone(),
                    coordinate,
                    geometric_distance_metres,
                },
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.geometric_distance_metres
            .cmp(&right.geometric_distance_metres)
            .then(left.station_id.as_str().cmp(right.station_id.as_str()))
    });
    candidates.truncate(max_candidates);
    candidates
}

fn endpoint_matrix_constraints(policy: &BikeAugmentationPolicy) -> RoutingConstraints {
    RoutingConstraints {
        time: None,
        max_distance: Some(Metres(policy.max_endpoint_walking_distance_metres)),
    }
}

fn accepted_endpoint_metric(
    cell: MatrixCell,
    max_duration_seconds: u32,
    max_distance_metres: u32,
) -> Option<BikeWalkingAccess> {
    let MatrixCell::Reachable { duration, distance } = cell else {
        return None;
    };
    (duration.0 > 0 && duration.0 <= max_duration_seconds && distance.0 <= max_distance_metres)
        .then(|| BikeWalkingAccess::new(duration.0, distance.0))
}

#[derive(Clone)]
struct VariantState {
    current_anchor: usize,
    current_time: OffsetDateTime,
    path: Vec<JourneyPathStep>,
    walking_seconds: u32,
    bicycle_seconds: u32,
    bicycle_distance_metres: u32,
    bike_share_uses: u32,
}

fn build_variants(
    skeleton: JourneySkeleton,
    shortcuts: Vec<BikeShortcut>,
    max_variants: usize,
) -> Vec<JourneyVariant> {
    let anchor_count = skeleton.anchors().len();
    let final_anchor = anchor_count.saturating_sub(1);
    let mut baseline_by_anchor = vec![Vec::new(); anchor_count];
    for (movement_index, movement) in skeleton.movements().iter().enumerate() {
        if movement.from_anchor < anchor_count {
            baseline_by_anchor[movement.from_anchor].push(movement_index);
        }
    }
    let mut shortcuts_by_anchor = vec![Vec::new(); anchor_count];
    for shortcut in shortcuts {
        if shortcut.from_anchor < final_anchor && shortcut.to_anchor <= final_anchor {
            shortcuts_by_anchor[shortcut.from_anchor].push(shortcut);
        }
    }
    for options in &mut shortcuts_by_anchor {
        options.sort_by(|left, right| {
            (
                left.to_anchor,
                left.total_seconds,
                left.pickup_station_id.as_str(),
            )
                .cmp(&(
                    right.to_anchor,
                    right.total_seconds,
                    right.pickup_station_id.as_str(),
                ))
        });
    }

    let start = skeleton.anchors()[0].reached_at;
    let mut frontiers = vec![Vec::new(); anchor_count];
    frontiers[0].push(VariantState {
        current_anchor: 0,
        current_time: start,
        path: Vec::new(),
        walking_seconds: 0,
        bicycle_seconds: 0,
        bicycle_distance_metres: 0,
        bike_share_uses: 0,
    });

    for anchor in 0..final_anchor {
        let states = std::mem::take(&mut frontiers[anchor]);
        for state in states {
            for movement_index in &baseline_by_anchor[anchor] {
                let movement = &skeleton.movements()[*movement_index];
                if movement.to_anchor != anchor + 1 {
                    continue;
                }
                if let Some(next) = advance_baseline(&state, movement_index, movement) {
                    insert_state(&mut frontiers[anchor + 1], next, max_variants);
                }
            }
            for shortcut in &shortcuts_by_anchor[anchor] {
                if let Some(next) = advance_shortcut(&state, shortcut) {
                    insert_state(&mut frontiers[shortcut.to_anchor], next, max_variants);
                }
            }
        }
    }

    let baseline = JourneyVariant::baseline(skeleton.clone());
    let mut variants = vec![baseline];
    for state in &frontiers[final_anchor] {
        if state.path == variants[0].path() {
            continue;
        }
        variants.push(JourneyVariant::from_path(
            skeleton.clone(),
            state.path.clone(),
        ));
    }
    variants.truncate(max_variants.max(1));
    variants
}

fn advance_baseline(
    state: &VariantState,
    movement_index: &usize,
    movement: &super::JourneyMovement,
) -> Option<VariantState> {
    let mut next = state.clone();
    match movement.kind {
        super::JourneyMovementKind::Walking {
            duration_seconds, ..
        } => {
            next.current_time += Duration::seconds(i64::from(duration_seconds));
            next.walking_seconds = next.walking_seconds.saturating_add(duration_seconds);
        }
        super::JourneyMovementKind::Transit { .. } => {
            if next.current_time > movement.departure_at {
                return None;
            }
            next.current_time = movement.arrival_at;
        }
    }
    next.current_anchor = movement.to_anchor;
    next.path.push(JourneyPathStep {
        from_anchor: movement.from_anchor,
        to_anchor: movement.to_anchor,
        movement: JourneyPathMovement::Baseline {
            movement_index: *movement_index,
        },
    });
    Some(next)
}

fn advance_shortcut(state: &VariantState, shortcut: &BikeShortcut) -> Option<VariantState> {
    let mut next = state.clone();
    next.current_time += Duration::seconds(i64::from(shortcut.total_seconds));
    next.current_anchor = shortcut.to_anchor;
    next.walking_seconds = next
        .walking_seconds
        .saturating_add(shortcut.walk_to_pickup.duration_seconds)
        .saturating_add(shortcut.walk_from_dropoff.duration_seconds);
    next.bicycle_seconds = next
        .bicycle_seconds
        .saturating_add(shortcut.bicycle_duration_seconds);
    next.bicycle_distance_metres = next
        .bicycle_distance_metres
        .saturating_add(shortcut.bicycle_distance_metres);
    next.bike_share_uses = next.bike_share_uses.saturating_add(1);
    next.path.push(JourneyPathStep {
        from_anchor: shortcut.from_anchor,
        to_anchor: shortcut.to_anchor,
        movement: JourneyPathMovement::BikeShare {
            shortcut: shortcut.clone(),
        },
    });
    Some(next)
}

fn insert_state(frontier: &mut Vec<VariantState>, candidate: VariantState, max_variants: usize) {
    if frontier.iter().any(|state| dominates(state, &candidate)) {
        return;
    }
    frontier.retain(|state| !dominates(&candidate, state));
    frontier.push(candidate);
    frontier.sort_by(|left, right| {
        (
            left.bike_share_uses,
            left.current_time,
            left.walking_seconds,
            left.bicycle_seconds,
            left.bicycle_distance_metres,
        )
            .cmp(&(
                right.bike_share_uses,
                right.current_time,
                right.walking_seconds,
                right.bicycle_seconds,
                right.bicycle_distance_metres,
            ))
    });
    frontier.truncate(max_variants.max(1));
}

fn dominates(left: &VariantState, right: &VariantState) -> bool {
    let no_worse = left.current_time <= right.current_time
        && left.walking_seconds <= right.walking_seconds
        && left.bicycle_seconds <= right.bicycle_seconds
        && left.bicycle_distance_metres <= right.bicycle_distance_metres
        && left.bike_share_uses <= right.bike_share_uses;
    let strictly_better = left.current_time < right.current_time
        || left.walking_seconds < right.walking_seconds
        || left.bicycle_seconds < right.bicycle_seconds
        || left.bicycle_distance_metres < right.bicycle_distance_metres
        || left.bike_share_uses < right.bike_share_uses;
    no_worse && strictly_better
}
