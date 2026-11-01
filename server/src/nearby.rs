use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use serde::Serialize;
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::domain::{
    bikeshare::{BikeAvailabilitySource, BikeShareNetwork},
    ids::{BikeStationId, BoardingPointId},
    location::GeoCoordinate,
    transit::TransitMode,
    transit_runtime::TransitRuntime,
};

pub const DEFAULT_RADIUS_METRES: u32 = 500;
pub const MAX_RADIUS_METRES: u32 = 2_000;
const MAX_TRANSIT_STOPS: usize = 40;
const MAX_BIKE_STATIONS: usize = 20;
const SAME_STOP_METRES: f64 = 50.0;

#[derive(Clone)]
pub struct NearbyService {
    transit: Arc<TransitRuntime>,
    services_by_stop: Arc<HashMap<BoardingPointId, StopServices>>,
    bike_share: Arc<BikeShareNetwork>,
    bike_availability: Arc<dyn BikeAvailabilitySource>,
}

#[derive(Default)]
struct StopServices {
    modes: BTreeSet<TransitMode>,
    route_labels: BTreeSet<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NearbyStops {
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub generated_at: OffsetDateTime,
    pub transit_stops: Vec<NearbyTransitStop>,
    pub bike_stations: Vec<NearbyBikeStation>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NearbyTransitStop {
    pub via_ids: Vec<String>,
    pub name: String,
    pub coordinate: GeoCoordinate,
    pub distance_metres: u32,
    pub modes: Vec<TransitMode>,
    pub route_labels: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NearbyBikeStation {
    pub via_id: String,
    pub name: String,
    pub coordinate: GeoCoordinate,
    pub distance_metres: u32,
    pub availability: Option<NearbyBikeAvailability>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NearbyBikeAvailability {
    pub available_bikes: u32,
    pub available_return_slots: u32,
    pub in_service: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub observed_at: OffsetDateTime,
}

impl NearbyService {
    pub(crate) fn new(
        transit: Arc<TransitRuntime>,
        bike_share: Arc<BikeShareNetwork>,
        bike_availability: Arc<dyn BikeAvailabilitySource>,
    ) -> Self {
        let mut services_by_stop: HashMap<BoardingPointId, StopServices> = HashMap::new();
        for pattern in &transit.network().patterns {
            let Some(route) = transit.route_for_pattern(pattern) else {
                continue;
            };
            for stop in &pattern.stops {
                let services = services_by_stop.entry(stop.clone()).or_default();
                services.modes.insert(route.mode);
                if !route.label.trim().is_empty() {
                    services.route_labels.insert(route.label.clone());
                }
            }
        }
        Self {
            transit,
            services_by_stop: Arc::new(services_by_stop),
            bike_share,
            bike_availability,
        }
    }

    pub async fn nearby(&self, center: GeoCoordinate, radius_metres: u32) -> NearbyStops {
        NearbyStops {
            generated_at: crate::domain::normalize_to_taipei(OffsetDateTime::now_utc()),
            transit_stops: self.transit_stops(center, radius_metres),
            bike_stations: self.bike_stations(center, radius_metres).await,
        }
    }

    fn transit_stops(&self, center: GeoCoordinate, radius_metres: u32) -> Vec<NearbyTransitStop> {
        let mut candidates: Vec<_> = self
            .transit
            .stops_within_radius(center, radius_metres)
            .into_iter()
            .filter_map(|stop| Some((stop, self.services_by_stop.get(&stop.id)?)))
            .map(|(stop, services)| (center.distance_metres(stop.coordinate), stop, services))
            .collect();
        candidates.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| a.1.id.as_str().cmp(b.1.id.as_str()))
        });

        let mut grouped: Vec<(NearbyTransitStop, StopServices)> = Vec::new();
        for (distance, stop, services) in candidates {
            let existing = grouped.iter_mut().find(|(group, _)| {
                group.name == stop.name
                    && group.coordinate.distance_metres(stop.coordinate) <= SAME_STOP_METRES
            });
            let (group, merged) = match existing {
                Some(entry) => entry,
                None => {
                    if grouped.len() == MAX_TRANSIT_STOPS {
                        continue;
                    }
                    grouped.push((
                        NearbyTransitStop {
                            via_ids: Vec::new(),
                            name: stop.name.clone(),
                            coordinate: stop.coordinate,
                            distance_metres: distance.round() as u32,
                            modes: Vec::new(),
                            route_labels: Vec::new(),
                        },
                        StopServices::default(),
                    ));
                    grouped.last_mut().expect("group was just pushed")
                }
            };
            group.via_ids.push(stop.id.as_str().to_owned());
            merged.modes.extend(services.modes.iter().copied());
            merged
                .route_labels
                .extend(services.route_labels.iter().cloned());
        }

        grouped
            .into_iter()
            .map(|(mut group, merged)| {
                group.modes = merged.modes.into_iter().collect();
                group.route_labels = merged.route_labels.into_iter().collect();
                group
            })
            .collect()
    }

    async fn bike_stations(
        &self,
        center: GeoCoordinate,
        radius_metres: u32,
    ) -> Vec<NearbyBikeStation> {
        let mut stations: Vec<_> = self
            .bike_share
            .stations_within_radius(center, f64::from(radius_metres))
            .into_iter()
            .map(|station| (center.distance_metres(station.coordinate), station))
            .collect();
        stations.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| a.1.id.as_str().cmp(b.1.id.as_str()))
        });
        stations.truncate(MAX_BIKE_STATIONS);
        if stations.is_empty() {
            return Vec::new();
        }

        let ids: Vec<BikeStationId> = stations
            .iter()
            .map(|(_, station)| station.id.clone())
            .collect();
        let snapshot = match self.bike_availability.snapshot(&ids).await {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tracing::warn!(
                    target: "via_server::nearby",
                    error = %error,
                    "bike-share availability unavailable for nearby stations"
                );
                None
            }
        };

        stations
            .into_iter()
            .map(|(distance, station)| NearbyBikeStation {
                via_id: station.id.as_str().to_owned(),
                name: station.name.clone(),
                coordinate: station.coordinate,
                distance_metres: distance.round() as u32,
                availability: snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.station(&station.id))
                    .map(|availability| NearbyBikeAvailability {
                        available_bikes: availability.available_bikes,
                        available_return_slots: availability.available_return_slots,
                        in_service: availability.in_service,
                        observed_at: availability.observed_at,
                    }),
            })
            .collect()
    }
}
