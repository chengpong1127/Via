//! Query-time BikeShare mobility paths, static connectors, and availability.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::api::RequestCoordinate;
use crate::domain::{
    ids::{BikeShareStationId, BoardingPointId},
    realtime::{ActionState, BikeAvailability},
    transit::{BikeShareConnector, TransitNetwork},
};
use crate::street::{
    Coordinate, MatrixCell, MatrixOrientation, MatrixQuery, Metres, RoutingConstraints,
    StreetRouter, StreetRoutingError, StreetTravelMode,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MobilityLeg {
    Walk {
        duration_seconds: u32,
        distance_metres: u32,
    },
    BikeShare {
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        duration_seconds: u32,
        distance_metres: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MobilityPath {
    pub legs: Vec<MobilityLeg>,
}

impl MobilityPath {
    pub fn walking(duration_seconds: u32, distance_metres: u32) -> Self {
        Self {
            legs: vec![MobilityLeg::Walk {
                duration_seconds,
                distance_metres,
            }],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn walking_and_bike(
        access_seconds: u32,
        access_metres: u32,
        pickup_station_id: BikeShareStationId,
        return_station_id: BikeShareStationId,
        bike_seconds: u32,
        bike_metres: u32,
        egress_seconds: u32,
        egress_metres: u32,
    ) -> Self {
        Self {
            legs: vec![
                MobilityLeg::Walk {
                    duration_seconds: access_seconds,
                    distance_metres: access_metres,
                },
                MobilityLeg::BikeShare {
                    pickup_station_id,
                    return_station_id,
                    duration_seconds: bike_seconds,
                    distance_metres: bike_metres,
                },
                MobilityLeg::Walk {
                    duration_seconds: egress_seconds,
                    distance_metres: egress_metres,
                },
            ],
        }
    }

    pub fn duration_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                MobilityLeg::Walk {
                    duration_seconds, ..
                }
                | MobilityLeg::BikeShare {
                    duration_seconds, ..
                } => *duration_seconds,
            })
        })
    }

    pub fn walking_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                MobilityLeg::Walk {
                    duration_seconds, ..
                } => *duration_seconds,
                MobilityLeg::BikeShare { .. } => 0,
            })
        })
    }

    pub fn bike_seconds(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                MobilityLeg::BikeShare {
                    duration_seconds, ..
                } => *duration_seconds,
                MobilityLeg::Walk { .. } => 0,
            })
        })
    }

    pub fn distance_metres(&self) -> u32 {
        self.legs.iter().fold(0, |total, leg| {
            total.saturating_add(match leg {
                MobilityLeg::Walk {
                    distance_metres, ..
                }
                | MobilityLeg::BikeShare {
                    distance_metres, ..
                } => *distance_metres,
            })
        })
    }

    pub fn required_station_ids(&self) -> Vec<BikeShareStationId> {
        let mut ids = self
            .legs
            .iter()
            .flat_map(|leg| match leg {
                MobilityLeg::BikeShare {
                    pickup_station_id,
                    return_station_id,
                    ..
                } => vec![pickup_station_id.clone(), return_station_id.clone()],
                MobilityLeg::Walk { .. } => Vec::new(),
            })
            .collect::<Vec<_>>();
        ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ids.dedup();
        ids
    }

    pub fn uses_bike_share(&self) -> bool {
        self.legs
            .iter()
            .any(|leg| matches!(leg, MobilityLeg::BikeShare { .. }))
    }
}

/// Legacy names retained for the existing fixture surface. New code should
/// use [`MobilityLeg`] and [`MobilityPath`].
pub type EndpointLeg = MobilityLeg;
pub type EndpointPath = MobilityPath;

/// Immutable, direction-aware index over artifact-backed transit/BikeShare
/// walking connectors.
#[derive(Clone, Debug, Default)]
pub struct BikeShareConnectorIndex {
    by_stop: HashMap<BoardingPointId, Vec<BikeShareConnector>>,
    by_station: HashMap<BikeShareStationId, Vec<BikeShareConnector>>,
}

impl BikeShareConnectorIndex {
    pub fn from_network(network: &TransitNetwork) -> Self {
        let mut index = Self::default();
        for connector in &network.bike_share_connectors {
            if connector.stop_to_station_seconds.is_some() {
                index
                    .by_stop
                    .entry(connector.stop_id.clone())
                    .or_default()
                    .push(connector.clone());
            }
            if connector.station_to_stop_seconds.is_some() {
                index
                    .by_station
                    .entry(connector.station_id.clone())
                    .or_default()
                    .push(connector.clone());
            }
        }
        for connectors in index.by_stop.values_mut() {
            connectors.sort_by(|left, right| {
                left.stop_to_station_seconds
                    .cmp(&right.stop_to_station_seconds)
                    .then(
                        left.stop_to_station_metres
                            .cmp(&right.stop_to_station_metres),
                    )
                    .then(left.station_id.as_str().cmp(right.station_id.as_str()))
            });
        }
        for connectors in index.by_station.values_mut() {
            connectors.sort_by(|left, right| {
                left.station_to_stop_seconds
                    .cmp(&right.station_to_stop_seconds)
                    .then(
                        left.station_to_stop_metres
                            .cmp(&right.station_to_stop_metres),
                    )
                    .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
            });
        }
        index
    }

    pub fn connectors_from_stop(&self, stop_id: &BoardingPointId) -> &[BikeShareConnector] {
        self.by_stop.get(stop_id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn connectors_from_station(
        &self,
        station_id: &BikeShareStationId,
    ) -> &[BikeShareConnector] {
        self.by_station
            .get(station_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.by_stop.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_stop.values().all(Vec::is_empty)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareTransfer {
    pub from_stop_id: BoardingPointId,
    pub to_stop_id: BoardingPointId,
    pub pickup_station_id: BikeShareStationId,
    pub return_station_id: BikeShareStationId,
    pub access_walk_seconds: u32,
    pub access_walk_metres: u32,
    pub bike_seconds: u32,
    pub bike_metres: u32,
    pub egress_walk_seconds: u32,
    pub egress_walk_metres: u32,
}

impl BikeShareTransfer {
    pub fn duration_seconds(&self) -> u32 {
        self.access_walk_seconds
            .saturating_add(self.bike_seconds)
            .saturating_add(self.egress_walk_seconds)
    }

    pub fn walking_seconds(&self) -> u32 {
        self.access_walk_seconds
            .saturating_add(self.egress_walk_seconds)
    }

    pub fn required_station_ids(&self) -> Vec<BikeShareStationId> {
        let mut ids = vec![
            self.pickup_station_id.clone(),
            self.return_station_id.clone(),
        ];
        ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ids.dedup();
        ids
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BikeShareExpansionQuery {
    pub from_stop_id: BoardingPointId,
    pub ready_at: OffsetDateTime,
    pub destination: Option<RequestCoordinate>,
    pub max_journey_duration_seconds: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BikeShareExpansion {
    pub transfers: Vec<BikeShareTransfer>,
    pub destination_paths: Vec<MobilityPath>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BikeShareOriginQuery {
    pub origin: RequestCoordinate,
    pub departure_at: OffsetDateTime,
    pub max_journey_duration_seconds: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareOriginAccess {
    pub stop_id: BoardingPointId,
    pub departure_at: OffsetDateTime,
    pub path: MobilityPath,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BikeShareDirectQuery {
    pub origin: RequestCoordinate,
    pub destination: RequestCoordinate,
    pub departure_at: OffsetDateTime,
    pub max_journey_duration_seconds: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BikeShareDirectPath {
    pub path: MobilityPath,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BikeShareOperatorMetrics {
    pub queries: usize,
    pub unique_source_stops: usize,
    pub pickup_stations_considered: usize,
    pub return_stations_considered: usize,
    pub bicycle_matrix_requests: usize,
    pub transfers_returned: usize,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BikeShareError {
    #[error("BikeShare operator has no usable static connector candidates")]
    NoCandidates,
    #[error("BikeShare matrix response was malformed")]
    MalformedMatrix,
    #[error("BikeShare street routing failed: {0}")]
    Street(#[from] StreetRoutingError),
    #[error("BikeShare availability is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("BikeShare availability response is invalid")]
    InvalidAvailability,
}

#[async_trait]
pub trait BikeShareOperator: Send + Sync {
    async fn expand_batch(
        &self,
        queries: Vec<BikeShareExpansionQuery>,
    ) -> Result<Vec<BikeShareExpansion>, BikeShareError>;

    async fn expand_origins_batch(
        &self,
        queries: Vec<BikeShareOriginQuery>,
    ) -> Result<Vec<Vec<BikeShareOriginAccess>>, BikeShareError>;

    async fn expand_direct_batch(
        &self,
        queries: Vec<BikeShareDirectQuery>,
    ) -> Result<Vec<Vec<BikeShareDirectPath>>, BikeShareError>;

    fn metrics(&self) -> BikeShareOperatorMetrics {
        BikeShareOperatorMetrics::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BikeShareOperatorLimits {
    pub max_pickup_stations_per_source: usize,
    pub max_return_stations_per_source: usize,
    pub max_final_transfers_per_source: usize,
    pub max_station_search_radius_metres: u32,
    pub max_walking_seconds: u32,
    pub max_walking_distance_metres: u32,
    pub max_bike_duration_seconds: u32,
    pub max_bike_distance_metres: u32,
}

impl Default for BikeShareOperatorLimits {
    fn default() -> Self {
        Self {
            max_pickup_stations_per_source: 6,
            max_return_stations_per_source: 8,
            max_final_transfers_per_source: 8,
            max_station_search_radius_metres: 2_000,
            max_walking_seconds: 8 * 60,
            max_walking_distance_metres: 800,
            max_bike_duration_seconds: 2 * 60 * 60,
            max_bike_distance_metres: 20_000,
        }
    }
}

/// The dynamic BikeShare operator. Static stop/station walking facts come from
/// [`BikeShareConnectorIndex`]; only bounded candidate discovery, realtime
/// availability, and bicycle matrix work happen at query time.
#[derive(Clone)]
pub struct StreetBikeShareOperator {
    network: Arc<TransitNetwork>,
    street: Arc<dyn StreetRouter>,
    connector_index: BikeShareConnectorIndex,
    availability: Option<Arc<dyn BikeAvailabilityResolver>>,
    limits: BikeShareOperatorLimits,
    metrics: Arc<Mutex<BikeShareOperatorMetrics>>,
}

impl StreetBikeShareOperator {
    pub fn new(
        network: Arc<TransitNetwork>,
        street: Arc<dyn StreetRouter>,
        availability: Option<Arc<dyn BikeAvailabilityResolver>>,
    ) -> Self {
        let connector_index = BikeShareConnectorIndex::from_network(&network);
        Self {
            network,
            street,
            connector_index,
            availability,
            limits: BikeShareOperatorLimits::default(),
            metrics: Arc::new(Mutex::new(BikeShareOperatorMetrics::default())),
        }
    }

    pub fn with_limits(mut self, limits: BikeShareOperatorLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn connector_index(&self) -> &BikeShareConnectorIndex {
        &self.connector_index
    }

    fn pickup_connectors(&self, stop_id: &BoardingPointId) -> Vec<BikeShareConnector> {
        self.connector_index
            .connectors_from_stop(stop_id)
            .iter()
            .filter(|connector| connector.stop_to_station_seconds.is_some())
            .take(self.limits.max_pickup_stations_per_source)
            .cloned()
            .collect()
    }

    fn return_stations(
        &self,
        reference: Coordinate,
        secondary_reference: Option<Coordinate>,
    ) -> Vec<BikeShareStationId> {
        let reference_request = crate::api::RequestCoordinate {
            latitude: reference.latitude(),
            longitude: reference.longitude(),
        };
        let mut stations = self
            .network
            .bike_share_stations_within_radius(
                reference_request,
                self.limits.max_station_search_radius_metres,
            )
            .into_iter()
            .filter(|station| {
                !self
                    .connector_index
                    .connectors_from_station(&station.id)
                    .is_empty()
            })
            .map(|station| station.id.clone())
            .collect::<Vec<_>>();
        if let Some(secondary) = secondary_reference {
            let secondary_request = crate::api::RequestCoordinate {
                latitude: secondary.latitude(),
                longitude: secondary.longitude(),
            };
            stations.extend(
                self.network
                    .bike_share_stations_within_radius(
                        secondary_request,
                        self.limits.max_station_search_radius_metres,
                    )
                    .into_iter()
                    .filter(|station| {
                        !self
                            .connector_index
                            .connectors_from_station(&station.id)
                            .is_empty()
                    })
                    .map(|station| station.id.clone()),
            );
        }
        stations.sort_by(|left, right| {
            station_distance(&self.network, left, reference)
                .cmp(&station_distance(&self.network, right, reference))
                .then(left.as_str().cmp(right.as_str()))
        });
        stations.dedup();
        stations.truncate(self.limits.max_return_stations_per_source);
        stations
    }

    async fn availability_for(
        &self,
        station_ids: impl IntoIterator<Item = BikeShareStationId>,
    ) -> Result<HashMap<BikeShareStationId, BikeAvailability>, BikeShareError> {
        let Some(resolver) = &self.availability else {
            return Ok(HashMap::new());
        };
        let mut station_ids = station_ids.into_iter().collect::<Vec<_>>();
        station_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        station_ids.dedup();
        if station_ids.is_empty() {
            return Ok(HashMap::new());
        }
        resolver
            .availability(&station_ids)
            .await
            .map_err(|error| match error {
                BikeAvailabilityError::TemporarilyUnavailable => {
                    BikeShareError::TemporarilyUnavailable
                }
                BikeAvailabilityError::InvalidResponse => BikeShareError::InvalidAvailability,
            })
    }

    async fn bicycle_matrix(
        &self,
        pickup_ids: &[BikeShareStationId],
        return_ids: &[BikeShareStationId],
    ) -> Result<
        (
            HashMap<(BikeShareStationId, BikeShareStationId), StreetMetric>,
            usize,
        ),
        BikeShareError,
    > {
        let pickup_ids = unique_ids(pickup_ids);
        let return_ids = unique_ids(return_ids);
        if pickup_ids.is_empty() || return_ids.is_empty() {
            return Ok((HashMap::new(), 0));
        }
        let limits = self.street.matrix_limits();
        let max_sources = limits.max_sources.max(1);
        let max_targets = limits.max_targets.max(1);
        let mut metrics = HashMap::new();
        let mut requests = 0;
        for pickups in pickup_ids.chunks(max_sources) {
            for returns in return_ids.chunks(max_targets) {
                requests += 1;
                let matrix = self
                    .street
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::ManyToMany,
                        travel_mode: StreetTravelMode::Bicycle,
                        sources: pickups
                            .iter()
                            .filter_map(|id| station_coordinate(&self.network, id))
                            .collect(),
                        targets: returns
                            .iter()
                            .filter_map(|id| station_coordinate(&self.network, id))
                            .collect(),
                        constraints: RoutingConstraints {
                            max_distance: Some(Metres(self.limits.max_bike_distance_metres)),
                            ..RoutingConstraints::default()
                        },
                    })
                    .await?;
                if matrix.cells.len() != pickups.len()
                    || matrix
                        .cells
                        .iter()
                        .any(|cells| cells.len() != returns.len())
                {
                    return Err(BikeShareError::MalformedMatrix);
                }
                for (pickup_index, cells) in matrix.cells.iter().enumerate() {
                    for (return_index, cell) in cells.iter().enumerate() {
                        let Some(metric) = bicycle_metric(cell, self.limits) else {
                            continue;
                        };
                        metrics.insert(
                            (pickups[pickup_index].clone(), returns[return_index].clone()),
                            metric,
                        );
                    }
                }
            }
        }
        Ok((metrics, requests))
    }

    async fn walking_from_stations_to_destination(
        &self,
        station_ids: &[BikeShareStationId],
        destination: Coordinate,
    ) -> Result<HashMap<BikeShareStationId, StreetMetric>, BikeShareError> {
        let station_ids = unique_ids(station_ids);
        let mut metrics = HashMap::new();
        for stations in station_ids.chunks(self.street.matrix_limits().max_sources.max(1)) {
            if stations.is_empty() {
                continue;
            }
            let matrix = self
                .street
                .matrix(MatrixQuery {
                    orientation: MatrixOrientation::ManyToOne,
                    travel_mode: StreetTravelMode::Walking,
                    sources: stations
                        .iter()
                        .filter_map(|id| station_coordinate(&self.network, id))
                        .collect(),
                    targets: vec![destination],
                    constraints: RoutingConstraints {
                        max_distance: Some(Metres(self.limits.max_walking_distance_metres)),
                        ..RoutingConstraints::default()
                    },
                })
                .await?;
            if matrix.cells.len() != stations.len()
                || matrix.cells.iter().any(|cells| cells.len() != 1)
            {
                return Err(BikeShareError::MalformedMatrix);
            }
            for (station, cells) in stations.iter().zip(&matrix.cells) {
                if let Some(metric) = walking_metric(&cells[0], self.limits) {
                    metrics.insert(station.clone(), metric);
                }
            }
        }
        Ok(metrics)
    }

    fn record_expansion_metrics(
        &self,
        queries: usize,
        source_stops: usize,
        pickups: usize,
        returns: usize,
        bicycle_matrix_requests: usize,
        transfers: usize,
    ) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.queries += queries;
            metrics.unique_source_stops += source_stops;
            metrics.pickup_stations_considered += pickups;
            metrics.return_stations_considered += returns;
            metrics.bicycle_matrix_requests += bicycle_matrix_requests;
            metrics.transfers_returned += transfers;
        }
    }
}

#[derive(Clone)]
struct ExpansionWork {
    query: BikeShareExpansionQuery,
    pickups: Vec<BikeShareConnector>,
    returns: Vec<BikeShareStationId>,
}

#[async_trait]
impl BikeShareOperator for StreetBikeShareOperator {
    async fn expand_batch(
        &self,
        queries: Vec<BikeShareExpansionQuery>,
    ) -> Result<Vec<BikeShareExpansion>, BikeShareError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique_work = Vec::new();
        let mut query_work = Vec::with_capacity(queries.len());
        let mut seen = HashMap::<String, usize>::new();
        for query in &queries {
            let key = expansion_key(query);
            let work_index = if let Some(index) = seen.get(&key) {
                *index
            } else {
                let source = self
                    .network
                    .stop(&query.from_stop_id)
                    .and_then(|stop| station_coordinate_for_coordinate(stop.coordinate));
                let Some(source) = source else {
                    query_work.push(None);
                    continue;
                };
                let pickups = self.pickup_connectors(&query.from_stop_id);
                let returns = self.return_stations(
                    source,
                    query
                        .destination
                        .and_then(|coordinate| street_coordinate_for_request(coordinate).ok()),
                );
                let index = unique_work.len();
                unique_work.push(ExpansionWork {
                    query: query.clone(),
                    pickups,
                    returns,
                });
                seen.insert(key, index);
                index
            };
            query_work.push(Some(work_index));
        }
        let pickup_ids = unique_work
            .iter()
            .flat_map(|work| {
                work.pickups
                    .iter()
                    .map(|connector| connector.station_id.clone())
            })
            .collect::<Vec<_>>();
        let return_ids = unique_work
            .iter()
            .flat_map(|work| work.returns.iter().cloned())
            .collect::<Vec<_>>();
        let station_ids = pickup_ids
            .iter()
            .chain(&return_ids)
            .cloned()
            .collect::<Vec<_>>();
        let availability = self.availability_for(station_ids).await?;
        let (bicycle, matrix_requests) = self.bicycle_matrix(&pickup_ids, &return_ids).await?;
        let mut destination_walks = HashMap::new();
        for work in &unique_work {
            if let Some(destination) = work
                .query
                .destination
                .and_then(|coordinate| street_coordinate_for_request(coordinate).ok())
            {
                let metrics = self
                    .walking_from_stations_to_destination(&work.returns, destination)
                    .await?;
                destination_walks.extend(metrics);
            }
        }

        let mut unique_results = Vec::with_capacity(unique_work.len());
        let mut total_transfers = 0;
        for work in &unique_work {
            let mut result = BikeShareExpansion::default();
            for pickup in &work.pickups {
                let (Some(access_seconds), Some(access_metres)) = (
                    pickup.stop_to_station_seconds,
                    pickup.stop_to_station_metres,
                ) else {
                    continue;
                };
                for return_id in &work.returns {
                    if pickup.station_id == *return_id
                        || !bike_availability_is_usable(
                            availability.get(&pickup.station_id),
                            availability.get(return_id),
                        )
                    {
                        continue;
                    }
                    let Some(bike) = bicycle.get(&(pickup.station_id.clone(), return_id.clone()))
                    else {
                        continue;
                    };
                    for connector in self.connector_index.connectors_from_station(return_id) {
                        if connector.stop_id == work.query.from_stop_id {
                            continue;
                        }
                        let (Some(egress_seconds), Some(egress_metres)) = (
                            connector.station_to_stop_seconds,
                            connector.station_to_stop_metres,
                        ) else {
                            continue;
                        };
                        let transfer = BikeShareTransfer {
                            from_stop_id: work.query.from_stop_id.clone(),
                            to_stop_id: connector.stop_id.clone(),
                            pickup_station_id: pickup.station_id.clone(),
                            return_station_id: return_id.clone(),
                            access_walk_seconds: access_seconds,
                            access_walk_metres: access_metres,
                            bike_seconds: bike.duration_seconds,
                            bike_metres: bike.distance_metres,
                            egress_walk_seconds: egress_seconds,
                            egress_walk_metres: egress_metres,
                        };
                        if transfer.duration_seconds() <= work.query.max_journey_duration_seconds {
                            result.transfers.push(transfer);
                        }
                    }
                    if let Some(destination_walk) = destination_walks.get(return_id) {
                        if work.query.destination.is_none() {
                            continue;
                        }
                        let path = MobilityPath::walking_and_bike(
                            access_seconds,
                            access_metres,
                            pickup.station_id.clone(),
                            return_id.clone(),
                            bike.duration_seconds,
                            bike.distance_metres,
                            destination_walk.duration_seconds,
                            destination_walk.distance_metres,
                        );
                        if path.duration_seconds() <= work.query.max_journey_duration_seconds {
                            result.destination_paths.push(path);
                        }
                    }
                }
            }
            sort_and_prune_expansion(&mut result, self.limits.max_final_transfers_per_source);
            total_transfers += result.transfers.len();
            unique_results.push(result);
        }
        self.record_expansion_metrics(
            queries.len(),
            unique_work
                .iter()
                .map(|work| work.query.from_stop_id.clone())
                .collect::<HashSet<_>>()
                .len(),
            pickup_ids.iter().collect::<HashSet<_>>().len(),
            return_ids.iter().collect::<HashSet<_>>().len(),
            matrix_requests,
            total_transfers,
        );
        Ok(query_work
            .into_iter()
            .map(|index| {
                index
                    .and_then(|index| unique_results.get(index).cloned())
                    .unwrap_or_default()
            })
            .collect())
    }

    async fn expand_origins_batch(
        &self,
        queries: Vec<BikeShareOriginQuery>,
    ) -> Result<Vec<Vec<BikeShareOriginAccess>>, BikeShareError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let mut pickup_ids = Vec::new();
        let mut return_ids = Vec::new();
        let mut per_query_pickups = Vec::new();
        let mut per_query_returns = Vec::new();
        for query in &queries {
            let origin_request = crate::api::RequestCoordinate {
                latitude: query.origin.latitude,
                longitude: query.origin.longitude,
            };
            let pickups = self
                .network
                .nearest_bike_share_stations(
                    origin_request,
                    self.limits.max_pickup_stations_per_source,
                )
                .into_iter()
                .map(|station| station.id.clone())
                .collect::<Vec<_>>();
            let returns = self.return_stations(street_coordinate_for_request(query.origin)?, None);
            pickup_ids.extend(pickups.iter().cloned());
            return_ids.extend(returns.iter().cloned());
            per_query_pickups.push(pickups);
            per_query_returns.push(returns);
        }
        let availability = self
            .availability_for(pickup_ids.iter().chain(&return_ids).cloned())
            .await?;
        let (bicycle, matrix_requests) = self.bicycle_matrix(&pickup_ids, &return_ids).await?;
        let mut all_results = Vec::with_capacity(queries.len());
        let mut walking_requests = 0;
        let mut total_transfers = 0;
        for (index, query) in queries.iter().enumerate() {
            let pickups = per_query_pickups.get(index).cloned().unwrap_or_default();
            let returns = per_query_returns.get(index).cloned().unwrap_or_default();
            if pickups.is_empty() || returns.is_empty() {
                all_results.push(Vec::new());
                continue;
            }
            let mut access_metrics = HashMap::new();
            for pickup_chunk in pickups.chunks(self.street.matrix_limits().max_targets.max(1)) {
                walking_requests += 1;
                let matrix = self
                    .street
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::OneToMany,
                        travel_mode: StreetTravelMode::Walking,
                        sources: vec![street_coordinate_for_request(query.origin)?],
                        targets: pickup_chunk
                            .iter()
                            .filter_map(|id| station_coordinate(&self.network, id))
                            .collect(),
                        constraints: RoutingConstraints {
                            max_distance: Some(Metres(self.limits.max_walking_distance_metres)),
                            ..RoutingConstraints::default()
                        },
                    })
                    .await?;
                if matrix.cells.len() != 1 || matrix.cells[0].len() != pickup_chunk.len() {
                    return Err(BikeShareError::MalformedMatrix);
                }
                for (station, cell) in pickup_chunk.iter().zip(&matrix.cells[0]) {
                    if let Some(metric) = walking_metric(cell, self.limits) {
                        access_metrics.insert(station.clone(), metric);
                    }
                }
            }
            let mut result = Vec::new();
            for pickup_id in &pickups {
                let Some(access) = access_metrics.get(pickup_id) else {
                    continue;
                };
                for return_id in &returns {
                    if pickup_id == return_id
                        || !bike_availability_is_usable(
                            availability.get(pickup_id),
                            availability.get(return_id),
                        )
                    {
                        continue;
                    }
                    let Some(bike) = bicycle.get(&(pickup_id.clone(), return_id.clone())) else {
                        continue;
                    };
                    for connector in self.connector_index.connectors_from_station(return_id) {
                        let (Some(egress_seconds), Some(egress_metres)) = (
                            connector.station_to_stop_seconds,
                            connector.station_to_stop_metres,
                        ) else {
                            continue;
                        };
                        let path = MobilityPath::walking_and_bike(
                            access.duration_seconds,
                            access.distance_metres,
                            pickup_id.clone(),
                            return_id.clone(),
                            bike.duration_seconds,
                            bike.distance_metres,
                            egress_seconds,
                            egress_metres,
                        );
                        if path.duration_seconds() <= query.max_journey_duration_seconds {
                            result.push(BikeShareOriginAccess {
                                stop_id: connector.stop_id.clone(),
                                departure_at: query.departure_at,
                                path,
                            });
                        }
                    }
                }
            }
            result.sort_by(|left, right| {
                left.path
                    .duration_seconds()
                    .cmp(&right.path.duration_seconds())
                    .then(left.stop_id.as_str().cmp(right.stop_id.as_str()))
                    .then(
                        station_signature(&left.path.required_station_ids())
                            .cmp(&station_signature(&right.path.required_station_ids())),
                    )
            });
            result.dedup();
            result.truncate(self.limits.max_final_transfers_per_source);
            total_transfers += result.len();
            all_results.push(result);
        }
        self.record_expansion_metrics(
            queries.len(),
            0,
            pickup_ids.iter().collect::<HashSet<_>>().len(),
            return_ids.iter().collect::<HashSet<_>>().len(),
            matrix_requests + walking_requests,
            total_transfers,
        );
        Ok(all_results)
    }

    async fn expand_direct_batch(
        &self,
        queries: Vec<BikeShareDirectQuery>,
    ) -> Result<Vec<Vec<BikeShareDirectPath>>, BikeShareError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let mut per_query_pickups = Vec::new();
        let mut per_query_returns = Vec::new();
        let mut pickup_ids = Vec::new();
        let mut return_ids = Vec::new();
        for query in &queries {
            let origin_request = crate::api::RequestCoordinate {
                latitude: query.origin.latitude,
                longitude: query.origin.longitude,
            };
            let destination_request = crate::api::RequestCoordinate {
                latitude: query.destination.latitude,
                longitude: query.destination.longitude,
            };
            let pickups = self
                .network
                .nearest_bike_share_stations(
                    origin_request,
                    self.limits.max_pickup_stations_per_source,
                )
                .into_iter()
                .map(|station| station.id.clone())
                .collect::<Vec<_>>();
            let returns = self
                .network
                .bike_share_stations_within_radius(
                    destination_request,
                    self.limits.max_station_search_radius_metres,
                )
                .into_iter()
                .map(|station| station.id.clone())
                .collect::<Vec<_>>();
            pickup_ids.extend(pickups.iter().cloned());
            return_ids.extend(returns.iter().cloned());
            per_query_pickups.push(pickups);
            per_query_returns.push(returns);
        }
        let availability = self
            .availability_for(pickup_ids.iter().chain(&return_ids).cloned())
            .await?;
        let (bicycle, matrix_requests) = self.bicycle_matrix(&pickup_ids, &return_ids).await?;
        let mut results = Vec::with_capacity(queries.len());
        let mut walking_requests = 0;
        for (index, query) in queries.iter().enumerate() {
            let pickups = per_query_pickups.get(index).cloned().unwrap_or_default();
            let returns = per_query_returns.get(index).cloned().unwrap_or_default();
            let mut access_metrics = HashMap::new();
            for chunk in pickups.chunks(self.street.matrix_limits().max_targets.max(1)) {
                if chunk.is_empty() {
                    continue;
                }
                walking_requests += 1;
                let matrix = self
                    .street
                    .matrix(MatrixQuery {
                        orientation: MatrixOrientation::OneToMany,
                        travel_mode: StreetTravelMode::Walking,
                        sources: vec![street_coordinate_for_request(query.origin)?],
                        targets: chunk
                            .iter()
                            .filter_map(|id| station_coordinate(&self.network, id))
                            .collect(),
                        constraints: RoutingConstraints {
                            max_distance: Some(Metres(self.limits.max_walking_distance_metres)),
                            ..RoutingConstraints::default()
                        },
                    })
                    .await?;
                if matrix.cells.len() != 1 || matrix.cells[0].len() != chunk.len() {
                    return Err(BikeShareError::MalformedMatrix);
                }
                for (station, cell) in chunk.iter().zip(&matrix.cells[0]) {
                    if let Some(metric) = walking_metric(cell, self.limits) {
                        access_metrics.insert(station.clone(), metric);
                    }
                }
            }
            let egress_metrics = self
                .walking_from_stations_to_destination(
                    &returns,
                    street_coordinate_for_request(query.destination)?,
                )
                .await?;
            let mut paths = Vec::new();
            for pickup_id in &pickups {
                let Some(access) = access_metrics.get(pickup_id) else {
                    continue;
                };
                for return_id in &returns {
                    if pickup_id == return_id
                        || !bike_availability_is_usable(
                            availability.get(pickup_id),
                            availability.get(return_id),
                        )
                    {
                        continue;
                    }
                    let (Some(bike), Some(egress)) = (
                        bicycle.get(&(pickup_id.clone(), return_id.clone())),
                        egress_metrics.get(return_id),
                    ) else {
                        continue;
                    };
                    let path = MobilityPath::walking_and_bike(
                        access.duration_seconds,
                        access.distance_metres,
                        pickup_id.clone(),
                        return_id.clone(),
                        bike.duration_seconds,
                        bike.distance_metres,
                        egress.duration_seconds,
                        egress.distance_metres,
                    );
                    if path.duration_seconds() <= query.max_journey_duration_seconds {
                        paths.push(BikeShareDirectPath { path });
                    }
                }
            }
            paths.sort_by(|left, right| {
                left.path
                    .duration_seconds()
                    .cmp(&right.path.duration_seconds())
                    .then(
                        station_signature(&left.path.required_station_ids())
                            .cmp(&station_signature(&right.path.required_station_ids())),
                    )
            });
            paths.dedup();
            paths.truncate(self.limits.max_final_transfers_per_source);
            results.push(paths);
        }
        self.record_expansion_metrics(
            queries.len(),
            0,
            pickup_ids.iter().collect::<HashSet<_>>().len(),
            return_ids.iter().collect::<HashSet<_>>().len(),
            matrix_requests + walking_requests,
            0,
        );
        Ok(results)
    }

    fn metrics(&self) -> BikeShareOperatorMetrics {
        self.metrics
            .lock()
            .map(|metrics| metrics.clone())
            .unwrap_or_default()
    }
}

fn expansion_key(query: &BikeShareExpansionQuery) -> String {
    let destination = query.destination.map(|coordinate| {
        format!(
            ":{}:{}",
            coordinate.latitude.to_bits(),
            coordinate.longitude.to_bits()
        )
    });
    format!(
        "{}:{}:{}{}",
        query.from_stop_id.as_str(),
        query.ready_at.unix_timestamp_nanos(),
        query.max_journey_duration_seconds,
        destination.unwrap_or_default()
    )
}

fn unique_ids(ids: &[BikeShareStationId]) -> Vec<BikeShareStationId> {
    let mut ids = ids.to_vec();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    ids.dedup();
    ids
}

fn station_signature(ids: &[BikeShareStationId]) -> Vec<&str> {
    ids.iter().map(|id| id.as_str()).collect()
}

fn station_coordinate(network: &TransitNetwork, id: &BikeShareStationId) -> Option<Coordinate> {
    network
        .bike_share_station(id)
        .and_then(|station| station_coordinate_for_coordinate(station.coordinate))
}

fn station_coordinate_for_coordinate(
    coordinate: crate::api::RequestCoordinate,
) -> Option<Coordinate> {
    Coordinate::try_new(coordinate.latitude, coordinate.longitude).ok()
}

fn street_coordinate_for_request(
    coordinate: RequestCoordinate,
) -> Result<Coordinate, BikeShareError> {
    Coordinate::try_new(coordinate.latitude, coordinate.longitude).map_err(BikeShareError::Street)
}

fn station_distance(
    network: &TransitNetwork,
    id: &BikeShareStationId,
    reference: Coordinate,
) -> u32 {
    let reference = crate::api::RequestCoordinate {
        latitude: reference.latitude(),
        longitude: reference.longitude(),
    };
    station_coordinate(network, id)
        .map(|coordinate| {
            u32::try_from(
                reference.planar_distance_metres(crate::api::RequestCoordinate {
                    latitude: coordinate.latitude(),
                    longitude: coordinate.longitude(),
                }),
            )
            .unwrap_or(u32::MAX)
        })
        .unwrap_or(u32::MAX)
}

#[derive(Clone, Copy)]
struct StreetMetric {
    duration_seconds: u32,
    distance_metres: u32,
}

fn walking_metric(cell: &MatrixCell, limits: BikeShareOperatorLimits) -> Option<StreetMetric> {
    let MatrixCell::Reachable { duration, distance } = cell else {
        return None;
    };
    (duration.0 <= limits.max_walking_seconds && distance.0 <= limits.max_walking_distance_metres)
        .then_some(StreetMetric {
            duration_seconds: duration.0,
            distance_metres: distance.0,
        })
}

fn bicycle_metric(cell: &MatrixCell, limits: BikeShareOperatorLimits) -> Option<StreetMetric> {
    let MatrixCell::Reachable { duration, distance } = cell else {
        return None;
    };
    (duration.0 > 0
        && duration.0 <= limits.max_bike_duration_seconds
        && distance.0 > 0
        && distance.0 <= limits.max_bike_distance_metres)
        .then_some(StreetMetric {
            duration_seconds: duration.0,
            distance_metres: distance.0,
        })
}

fn sort_and_prune_expansion(result: &mut BikeShareExpansion, limit: usize) {
    result.transfers.sort_by(|left, right| {
        left.duration_seconds()
            .cmp(&right.duration_seconds())
            .then(left.to_stop_id.as_str().cmp(right.to_stop_id.as_str()))
            .then(
                left.pickup_station_id
                    .as_str()
                    .cmp(right.pickup_station_id.as_str()),
            )
            .then(
                left.return_station_id
                    .as_str()
                    .cmp(right.return_station_id.as_str()),
            )
    });
    result.transfers.dedup();
    result.transfers.truncate(limit.max(1));
    result.destination_paths.sort_by(|left, right| {
        left.duration_seconds().cmp(&right.duration_seconds()).then(
            station_signature(&left.required_station_ids())
                .cmp(&station_signature(&right.required_station_ids())),
        )
    });
    result.destination_paths.dedup();
    result.destination_paths.truncate(limit.max(1));
}

#[async_trait]
pub trait BikeAvailabilityResolver: Send + Sync {
    async fn availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<HashMap<BikeShareStationId, BikeAvailability>, BikeAvailabilityError>;
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BikeAvailabilityError {
    #[error("bike-share availability is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("bike-share availability response is invalid")]
    InvalidResponse,
}

pub fn bike_availability_is_usable(
    pickup: Option<&BikeAvailability>,
    return_station: Option<&BikeAvailability>,
) -> bool {
    pickup.is_none_or(|availability| {
        availability.rental_state != ActionState::Disabled
            && availability.available_bikes != Some(0)
    }) && return_station.is_none_or(|availability| {
        availability.return_state != ActionState::Disabled
            && availability.available_return_slots != Some(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::RequestCoordinate,
        domain::transit::{BikeShareStation, TransitStop},
        street::FakeStreetRouter,
    };

    #[test]
    fn connector_index_keeps_directional_station_and_stop_fanout() {
        let stop = BoardingPointId::new("stop");
        let station = BikeShareStationId::new("station");
        let network = TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            vec![TransitStop::new(
                stop.clone(),
                "stop",
                RequestCoordinate {
                    latitude: 25.0,
                    longitude: 121.0,
                },
            )],
            Vec::new(),
            Vec::new(),
            vec![BikeShareStation::new(
                station.clone(),
                "station",
                RequestCoordinate {
                    latitude: 25.0,
                    longitude: 121.0,
                },
            )],
        )
        .with_bike_share_connectors(vec![
            BikeShareConnector::new(stop.clone(), station.clone())
                .with_stop_to_station(90, 100)
                .with_station_to_stop(120, 140),
        ]);
        let index = BikeShareConnectorIndex::from_network(&network);
        assert_eq!(index.connectors_from_stop(&stop).len(), 1);
        assert_eq!(index.connectors_from_station(&station).len(), 1);
        assert_eq!(
            index.connectors_from_stop(&stop)[0].stop_to_station_seconds,
            Some(90)
        );
        assert_eq!(
            index.connectors_from_station(&station)[0].station_to_stop_seconds,
            Some(120)
        );
    }

    #[tokio::test]
    async fn operator_batches_bicycle_expansion_and_returns_transfer_and_destination_paths() {
        let stop_a = BoardingPointId::new("a");
        let stop_b = BoardingPointId::new("b");
        let pickup = BikeShareStationId::new("pickup");
        let return_station = BikeShareStationId::new("return");
        let a = RequestCoordinate {
            latitude: 25.0,
            longitude: 121.0,
        };
        let b = RequestCoordinate {
            latitude: 25.0002,
            longitude: 121.0,
        };
        let pickup_coordinate = RequestCoordinate {
            latitude: 25.0001,
            longitude: 121.0,
        };
        let return_coordinate = RequestCoordinate {
            latitude: 25.0003,
            longitude: 121.0,
        };
        let network = Arc::new(
            TransitNetwork::new(
                time::Time::from_hms(4, 0, 0).unwrap(),
                vec![
                    TransitStop::new(stop_a.clone(), "a", a),
                    TransitStop::new(stop_b.clone(), "b", b),
                ],
                Vec::new(),
                Vec::new(),
                vec![
                    BikeShareStation::new(pickup.clone(), "pickup", pickup_coordinate),
                    BikeShareStation::new(return_station.clone(), "return", return_coordinate),
                ],
            )
            .with_bike_share_connectors(vec![
                BikeShareConnector::new(stop_a.clone(), pickup.clone())
                    .with_stop_to_station(60, 100),
                BikeShareConnector::new(stop_b.clone(), return_station.clone())
                    .with_station_to_stop(75, 120),
            ]),
        );
        let pickup_coordinate =
            Coordinate::try_new(pickup_coordinate.latitude, pickup_coordinate.longitude).unwrap();
        let return_coordinate =
            Coordinate::try_new(return_coordinate.latitude, return_coordinate.longitude).unwrap();
        let destination = Coordinate::try_new(b.latitude, b.longitude).unwrap();
        let street = FakeStreetRouter::builder()
            .matrix_leg(
                StreetTravelMode::Bicycle,
                pickup_coordinate,
                return_coordinate,
                crate::street::Seconds(180),
                crate::street::Metres(900),
            )
            .matrix_leg(
                StreetTravelMode::Walking,
                return_coordinate,
                destination,
                crate::street::Seconds(75),
                crate::street::Metres(120),
            )
            .build();
        let operator = StreetBikeShareOperator::new(network, Arc::new(street), None);
        let results = operator
            .expand_batch(vec![
                BikeShareExpansionQuery {
                    from_stop_id: stop_a.clone(),
                    ready_at: time::macros::datetime!(2026-08-31 10:00 UTC),
                    destination: Some(b),
                    max_journey_duration_seconds: 900,
                },
                BikeShareExpansionQuery {
                    from_stop_id: stop_a,
                    ready_at: time::macros::datetime!(2026-08-31 10:00 UTC),
                    destination: Some(b),
                    max_journey_duration_seconds: 900,
                },
            ])
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].transfers.len(), 1);
        assert_eq!(results[0].transfers[0].to_stop_id, stop_b);
        assert_eq!(results[0].destination_paths.len(), 1);
        assert_eq!(operator.metrics().bicycle_matrix_requests, 1);
    }
}
