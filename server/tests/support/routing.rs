use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::{Duration, Time};
use via_server::domain::ids::{
    BoardingPointId, EffectiveTripId, ScheduledTripId, TransitServiceId,
};
use via_server::domain::transit::{TransitMode, TransitNetwork, TransitService, TransitStop};
use via_server::domain::transit_timing::{
    DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
    TimingBasis, TimingReliability,
};
use via_server::routing::{AccessPoint, AlgorithmQuery, EgressPoint};
use via_server::street::{
    Coordinate, FakeStreetRouter, Maneuver, Metres, Seconds, StreetPath, StreetTravelMode,
};

use crate::support::network::FIXTURE_DEPARTURE_UTC;

pub fn coordinate(latitude: f64, longitude: f64) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

pub fn path(from: Coordinate, to: Coordinate, duration: u32) -> StreetPath {
    StreetPath {
        duration: Seconds(duration),
        distance: Metres(duration * 5),
        geometry: vec![from, to],
        maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
        attribution: Vec::new(),
    }
}

pub fn stop_coordinate(network: &TransitNetwork, id: &str) -> Coordinate {
    let stop: &TransitStop = network.stop(&BoardingPointId::new(id)).unwrap();
    coordinate(stop.coordinate.latitude, stop.coordinate.longitude)
}

pub fn near_stop(network: &TransitNetwork, id: &str) -> Coordinate {
    let stop = stop_coordinate(network, id);
    coordinate(stop.latitude() + 0.0001, stop.longitude() + 0.0001)
}

pub fn transit_street(
    network: &TransitNetwork,
    origin: Coordinate,
    access_stop: &str,
    destination: Coordinate,
    egress_stop: &str,
) -> FakeStreetRouter {
    let access = stop_coordinate(network, access_stop);
    let egress = stop_coordinate(network, egress_stop);
    FakeStreetRouter::builder()
        .path(
            StreetTravelMode::Walking,
            origin,
            access,
            path(origin, access, 60),
        )
        .path(
            StreetTravelMode::Walking,
            egress,
            destination,
            path(egress, destination, 60),
        )
        .build()
}

pub fn stop(id: &str) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        id,
        via_server::api::RequestCoordinate {
            latitude: 25.0,
            longitude: 121.5,
        },
    )
}

pub fn routing_network(service_ids_and_patterns: &[(&str, &[&str])]) -> Arc<TransitNetwork> {
    routing_network_with_mode(service_ids_and_patterns, TransitMode::Bus)
}

pub fn routing_network_with_mode(
    service_ids_and_patterns: &[(&str, &[&str])],
    mode: TransitMode,
) -> Arc<TransitNetwork> {
    let stop_ids = service_ids_and_patterns
        .iter()
        .flat_map(|(_, pattern)| pattern.iter().copied())
        .collect::<std::collections::BTreeSet<_>>();
    let stops = stop_ids.into_iter().map(stop).collect::<Vec<_>>();
    let services = service_ids_and_patterns
        .iter()
        .map(|(service_id, pattern)| {
            TransitService::new(
                TransitServiceId::new(*service_id),
                mode,
                *service_id,
                pattern.last().copied().unwrap_or_default(),
                pattern.iter().map(|id| BoardingPointId::new(*id)).collect(),
            )
        })
        .collect();
    Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        stops,
        services,
        Vec::new(),
    ))
}

pub fn trip(
    id: &str,
    service_id: &str,
    stops: &[&str],
    departures: &[i64],
    arrivals: &[i64],
    realtime: bool,
) -> EffectiveTrip {
    let stop_times = stops
        .iter()
        .enumerate()
        .map(|(index, stop_id)| EffectiveStopTime {
            stop_id: BoardingPointId::new(*stop_id),
            arrival_at: FIXTURE_DEPARTURE_UTC + Duration::seconds(arrivals[index]),
            departure_at: FIXTURE_DEPARTURE_UTC + Duration::seconds(departures[index]),
        })
        .collect();
    EffectiveTrip {
        id: EffectiveTripId::new(id),
        service_id: TransitServiceId::new(service_id),
        scheduled_trip_id: (!realtime).then(|| ScheduledTripId::new(id)),
        stop_times,
        timing_basis: if realtime {
            TimingBasis::RealtimePrediction
        } else {
            TimingBasis::Schedule
        },
        timing_reliability: if realtime {
            TimingReliability::Reliable
        } else {
            TimingReliability::Fallback
        },
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    }
}

#[derive(Clone, Default)]
pub struct FixtureResolver {
    pub trips: Arc<HashMap<TransitServiceId, Vec<EffectiveTrip>>>,
    pub calls: Arc<Mutex<Vec<DepartureQuery>>>,
}

#[async_trait]
impl DepartureResolver for FixtureResolver {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.calls.lock().unwrap().push(query.clone());
        let Some(trips) = self.trips.get(&query.service_id) else {
            return Ok(Vec::new());
        };
        let mut result = trips
            .iter()
            .filter(|trip| {
                trip.stop_index(&query.board_stop_id)
                    .is_some_and(|index| trip.stop_times[index].departure_at >= query.after)
            })
            .cloned()
            .collect::<Vec<_>>();
        result.truncate(query.limit);
        Ok(result)
    }
}

pub fn resolver(trips: Vec<EffectiveTrip>) -> FixtureResolver {
    let service_id = trips[0].service_id.clone();
    FixtureResolver {
        trips: Arc::new(HashMap::from([(service_id, trips)])),
        ..Default::default()
    }
}

pub fn algorithm_query(from: &str, to: &str) -> AlgorithmQuery {
    AlgorithmQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new(from),
            FIXTURE_DEPARTURE_UTC,
            FIXTURE_DEPARTURE_UTC,
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new(to), 0, 0)],
        FIXTURE_DEPARTURE_UTC,
    )
}
