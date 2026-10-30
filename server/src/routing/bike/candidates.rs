use super::*;

impl BikeShareMovementPlanner {
    pub(crate) fn required_station_ids<'a>(
        &self,
        anchors: impl IntoIterator<Item = &'a CorridorAnchor>,
    ) -> Vec<BikeStationId> {
        let mut ids = HashSet::new();
        let mut seen = HashSet::new();
        for anchor in anchors {
            if !seen.insert(anchor.cache_key()) {
                continue;
            }
            if let Some(id) = anchor.boarding_point() {
                ids.extend(
                    self.network
                        .access_from_boarding_point(id)
                        .iter()
                        .filter(|access| {
                            access.walk_to_station.is_some() || access.walk_from_station.is_some()
                        })
                        .map(|access| access.station_id.clone()),
                );
            } else {
                ids.extend(
                    self.network
                        .stations_within_radius(
                            endpoint_coordinate(anchor),
                            f64::from(self.settings.endpoint_radius_metres),
                        )
                        .into_iter()
                        .map(|station| station.id.clone()),
                );
            }
        }
        let mut ids = ids.into_iter().collect::<Vec<_>>();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    pub(crate) fn movement_candidate(
        &self,
        from: &CorridorAnchor,
        to: &CorridorAnchor,
        snapshot: &BikeAvailabilitySnapshot,
        now: OffsetDateTime,
    ) -> Option<BikeMovementCandidate> {
        let view = self.network.availability_view_with_policy(
            snapshot,
            now,
            time::Duration::seconds(i64::from(self.settings.availability_max_age_seconds)),
            time::Duration::seconds(i64::from(self.settings.availability_max_clock_skew_seconds)),
        );
        let pickups = endpoint_station_choices(&self.network, &view, from, true, &self.settings);
        let dropoffs = endpoint_station_choices(&self.network, &view, to, false, &self.settings);
        if pickups.is_empty() || dropoffs.is_empty() {
            return None;
        }

        if !pickups.iter().any(|pickup| {
            dropoffs
                .iter()
                .any(|dropoff| pickup.station_id != dropoff.station_id)
        }) {
            return None;
        }

        Some(BikeMovementCandidate {
            pickups,
            dropoffs,
            pickup_endpoint: from.boarding_point().is_none().then_some(from.coordinate),
            dropoff_endpoint: to.boarding_point().is_none().then_some(to.coordinate),
        })
    }
}

#[derive(Clone)]
pub(super) struct StationChoice {
    pub(super) station_id: BikeStationId,
    pub(super) walking: BikeWalkingAccess,
}

pub(super) fn station_choices<'a>(
    accesses: impl IntoIterator<Item = &'a BikeStationAccess>,
    pickup: bool,
    settings: &BikeMovementPolicy,
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
        settings.max_pickup_stations_per_anchor
    } else {
        settings.max_dropoff_stations_per_anchor
    });
    choices
}

fn endpoint_coordinate(endpoint: &CorridorAnchor) -> GeoCoordinate {
    GeoCoordinate {
        latitude: endpoint.coordinate.latitude(),
        longitude: endpoint.coordinate.longitude(),
    }
}

fn endpoint_station_choices(
    network: &BikeShareNetwork,
    view: &crate::domain::bikeshare::BikeAvailabilityView<'_>,
    endpoint: &CorridorAnchor,
    pickup: bool,
    settings: &BikeMovementPolicy,
) -> Vec<StationChoice> {
    if let Some(id) = endpoint.boarding_point() {
        return station_choices(
            if pickup {
                view.usable_pickups(id)
            } else {
                view.usable_dropoffs(id)
            },
            pickup,
            settings,
        );
    }
    let point = endpoint_coordinate(endpoint);
    let mut choices = network
        .stations_within_radius(point, f64::from(settings.endpoint_radius_metres))
        .into_iter()
        .filter(|station| {
            if pickup {
                view.can_pick_up_at(&station.id)
            } else {
                view.can_return_at(&station.id)
            }
        })
        .map(|station| {
            let distance = point.distance_metres(station.coordinate);
            StationChoice {
                station_id: station.id.clone(),
                walking: BikeWalkingAccess::new(
                    (distance / settings.optimistic_walk_speed_metres_per_second).floor() as u32,
                    distance.floor() as u32,
                ),
            }
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.walking
            .distance_metres
            .cmp(&right.walking.distance_metres)
            .then(left.station_id.as_str().cmp(right.station_id.as_str()))
    });
    choices.truncate(settings.max_stations_per_endpoint);
    choices
}
