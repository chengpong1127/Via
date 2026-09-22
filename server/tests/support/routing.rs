use via_server::domain::transit::{TransitNetwork, TransitStop};
use via_server::street::{
    Coordinate, FakeStreetRouter, Maneuver, Metres, Seconds, StreetPath, StreetTravelMode,
};

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
    let stop: &TransitStop = network
        .stop(&via_server::domain::ids::BoardingPointId::new(id))
        .unwrap();
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
