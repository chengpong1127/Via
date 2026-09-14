#![allow(dead_code)]

use via_server::street::{
    Coordinate, ManeuverKind, MatrixCell, MatrixOrientation, MatrixQuery, Metres, RouteQuery,
    Seconds, StreetPath, StreetRouter, StreetRoutingError, StreetTravelMode,
};

pub struct ProviderContractFixture {
    pub origin: Coordinate,
    pub destination: Coordinate,
    pub unreachable: Coordinate,
    pub alternate_origin: Coordinate,
    pub duration: Seconds,
    pub distance: Metres,
    pub geometry: Vec<Coordinate>,
    pub maneuver_kinds: Vec<ManeuverKind>,
}

pub async fn assert_provider_contract(
    router: &dyn StreetRouter,
    fixture: &ProviderContractFixture,
) -> StreetPath {
    let matrix = router
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Bicycle,
            sources: vec![fixture.origin],
            targets: vec![fixture.destination, fixture.unreachable],
        })
        .await
        .unwrap();
    assert_eq!(
        matrix.cells[0][0],
        MatrixCell::Reachable {
            duration: fixture.duration,
            distance: fixture.distance,
        }
    );
    assert_eq!(matrix.cells[0][1], MatrixCell::Unreachable);

    let walking_matrix = router
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::ManyToOne,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![fixture.origin, fixture.alternate_origin],
            targets: vec![fixture.destination],
        })
        .await
        .unwrap();
    assert!(matches!(
        walking_matrix.cells[0][0],
        MatrixCell::Reachable { .. }
    ));
    assert!(matches!(
        walking_matrix.cells[1][0],
        MatrixCell::Reachable { .. }
    ));

    let path = router
        .route(RouteQuery {
            travel_mode: StreetTravelMode::Bicycle,
            constraints: Default::default(),
            origin: fixture.origin,
            destination: fixture.destination,
        })
        .await
        .unwrap();
    assert_eq!(path.duration, fixture.duration);
    assert_eq!(path.distance, fixture.distance);
    assert_eq!(path.geometry, fixture.geometry);
    assert_eq!(
        path.maneuvers
            .iter()
            .map(|maneuver| maneuver.kind)
            .collect::<Vec<_>>(),
        fixture.maneuver_kinds
    );
    path
}

pub fn assert_contract_error(actual: StreetRoutingError, expected: StreetRoutingError) {
    assert_eq!(actual, expected);
}

pub fn assert_timeout(error: StreetRoutingError) {
    assert_contract_error(error, StreetRoutingError::Timeout);
}

pub fn assert_malformed_response(error: StreetRoutingError) {
    assert_contract_error(error, StreetRoutingError::MalformedResponse);
}

pub fn assert_unreachable(error: StreetRoutingError) {
    assert_contract_error(error, StreetRoutingError::Unreachable);
}

pub fn assert_rate_limited(error: StreetRoutingError) {
    assert_contract_error(error, StreetRoutingError::RateLimited);
}

pub fn assert_cancellation_follow_up_succeeds<T>(result: Result<T, StreetRoutingError>) {
    assert!(result.is_ok(), "cancellation poisoned the street router");
}
