use via_server::street::{Coordinate, Maneuver, Metres, Seconds, StreetPath};

pub fn coordinate(latitude: f64, longitude: f64) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

pub fn straight_path(
    from: Coordinate,
    to: Coordinate,
    duration: Seconds,
    distance: Metres,
) -> StreetPath {
    StreetPath {
        duration,
        distance,
        geometry: vec![from, to],
        maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
        attribution: Vec::new(),
    }
}
