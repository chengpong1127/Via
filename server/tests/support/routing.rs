use via_server::domain::transit::network::{TransitNetwork, TransitStop};
use via_server::street::{
    Coordinate, FakeStreetRouter, Metres, Seconds, StreetPath, StreetTravelMode,
};

pub use super::street::coordinate;
use super::street::straight_path;

pub fn path(from: Coordinate, to: Coordinate, duration: u32) -> StreetPath {
    straight_path(from, to, Seconds(duration), Metres(duration * 5))
}

pub fn stop_coordinate(network: &TransitNetwork, id: &str) -> Coordinate {
    let stop: &TransitStop = network
        .stops
        .iter()
        .find(|stop| stop.id.as_str() == id)
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
    let mut builder = FakeStreetRouter::builder();
    for transfer in &network.transfer_edges {
        let from = stop_coordinate(network, transfer.from.as_str());
        let to = stop_coordinate(network, transfer.to.as_str());
        builder = builder.path(
            StreetTravelMode::Walking,
            from,
            to,
            path(from, to, transfer.duration_seconds),
        );
    }
    builder
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
