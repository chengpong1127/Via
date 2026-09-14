mod support;

use std::sync::Arc;

use support::{ProviderContractFixture, assert_provider_contract};
use via_server::street::{
    Coordinate, FakeStreetRouter, Maneuver, ManeuverKind, MatrixCell, MatrixOrientation,
    MatrixQuery, Metres, RouteQuery, Seconds, StreetPath, StreetRouter, StreetRoutingError,
    StreetTravelMode,
};

fn coordinate(latitude: f64, longitude: f64) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

fn path(origin: Coordinate, destination: Coordinate) -> StreetPath {
    StreetPath {
        duration: Seconds(620),
        distance: Metres(3_900),
        geometry: vec![origin, destination],
        maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
        attribution: Vec::new(),
    }
}

#[tokio::test]
async fn fake_street_router_preserves_matrix_order_and_route_details() {
    let origin = coordinate(25.04776, 121.51706);
    let destination = coordinate(25.03396, 121.56447);
    let unreachable = coordinate(25.03344, 121.53409);
    let alternate_origin = coordinate(25.05, 121.52);
    let bicycle_path = path(origin, destination);
    let walking_path = path(alternate_origin, destination);
    let router = FakeStreetRouter::builder()
        .matrix_leg(
            StreetTravelMode::Bicycle,
            origin,
            destination,
            Seconds(620),
            Metres(3_900),
        )
        .matrix_leg(
            StreetTravelMode::Walking,
            origin,
            destination,
            Seconds(620),
            Metres(3_900),
        )
        .matrix_leg(
            StreetTravelMode::Walking,
            alternate_origin,
            destination,
            Seconds(700),
            Metres(4_100),
        )
        .unreachable_leg(StreetTravelMode::Bicycle, origin, unreachable)
        .path(StreetTravelMode::Bicycle, origin, destination, bicycle_path)
        .path(
            StreetTravelMode::Walking,
            alternate_origin,
            destination,
            walking_path,
        )
        .build();

    let path = assert_provider_contract(
        &router,
        &ProviderContractFixture {
            origin,
            destination,
            unreachable,
            alternate_origin,
            duration: Seconds(620),
            distance: Metres(3_900),
            geometry: vec![origin, destination],
            maneuver_kinds: vec![ManeuverKind::Depart, ManeuverKind::Arrive],
        },
    )
    .await;
    assert_eq!(path.duration, Seconds(620));
}

#[tokio::test]
async fn matrix_shape_validation_is_semantic_not_provider_sized() {
    let origin = coordinate(25.0, 121.5);
    let targets = (0..33)
        .map(|index| coordinate(25.0 + f64::from(index) / 10_000.0, 121.5))
        .collect::<Vec<_>>();
    let router = FakeStreetRouter::builder().build();
    let result = router
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            travel_mode: StreetTravelMode::Walking,
            sources: vec![origin],
            targets,
            constraints: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(result.cells[0].len(), 33);
}

#[tokio::test]
async fn fake_router_normalizes_failures_and_max_distance() {
    let origin = coordinate(25.0, 121.5);
    let destination = coordinate(25.1, 121.6);
    let router = FakeStreetRouter::builder()
        .matrix_failure(StreetRoutingError::Timeout)
        .route_failure(StreetRoutingError::MalformedResponse)
        .path(
            StreetTravelMode::Walking,
            origin,
            destination,
            path(origin, destination),
        )
        .build();
    let matrix_error = router
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            travel_mode: StreetTravelMode::Walking,
            sources: vec![origin],
            targets: vec![destination],
            constraints: Default::default(),
        })
        .await
        .unwrap_err();
    assert_eq!(matrix_error, StreetRoutingError::Timeout);
    let route_error = router
        .route(RouteQuery {
            travel_mode: StreetTravelMode::Walking,
            origin,
            destination,
            constraints: Default::default(),
        })
        .await
        .unwrap_err();
    assert_eq!(route_error, StreetRoutingError::MalformedResponse);

    let limited_router = FakeStreetRouter::builder()
        .path(
            StreetTravelMode::Walking,
            origin,
            destination,
            path(origin, destination),
        )
        .build();
    let limited = limited_router
        .route(RouteQuery {
            travel_mode: StreetTravelMode::Walking,
            origin,
            destination,
            constraints: via_server::street::RoutingConstraints {
                max_distance: Some(Metres(100)),
                ..Default::default()
            },
        })
        .await
        .unwrap_err();
    assert_eq!(limited, StreetRoutingError::Unreachable);
}

#[tokio::test]
async fn coordinate_rejects_non_finite_and_out_of_range_values() {
    assert!(matches!(
        Coordinate::try_new(91.0, 121.5),
        Err(StreetRoutingError::InvalidRequest { .. })
    ));
    assert!(matches!(
        Coordinate::try_new(f64::NAN, 121.5),
        Err(StreetRoutingError::InvalidRequest { .. })
    ));
}

#[tokio::test]
async fn cancelling_a_fake_operation_does_not_require_a_session() {
    let router: Arc<dyn StreetRouter> = Arc::new(FakeStreetRouter::builder().build());
    let result = router
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            travel_mode: StreetTravelMode::Walking,
            sources: vec![coordinate(25.0, 121.5)],
            targets: vec![coordinate(25.1, 121.6)],
            constraints: Default::default(),
        })
        .await;
    assert!(result.is_ok());
    assert!(matches!(
        result.unwrap().cells[0][0],
        MatrixCell::Unreachable
    ));
}
