use via_server::street::{
    Coordinate, DetailedPath, DetailedPathQuery, ManeuverKind, MatrixOrientation, MatrixQuery,
    MatrixReachability, Metres, ProviderError, Seconds, StreetRoutingSession, StreetTravelMode,
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
    session: &StreetRoutingSession,
    fixture: &ProviderContractFixture,
) -> DetailedPath {
    let correlations = session.correlate(vec![fixture.origin]).await.unwrap();
    assert_eq!(correlations.len(), 1);

    let matrix = session
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
        matrix.cells[0][0].reachability,
        MatrixReachability::Reachable {
            duration: fixture.duration,
            distance: fixture.distance,
        }
    );
    assert_eq!(
        matrix.cells[0][1].reachability,
        MatrixReachability::Unreachable
    );

    let walking_matrix = session
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
        walking_matrix.cells[0][0].reachability,
        MatrixReachability::Reachable { .. }
    ));
    assert!(matches!(
        walking_matrix.cells[1][0].reachability,
        MatrixReachability::Reachable { .. }
    ));

    let path = session
        .detailed_path(DetailedPathQuery {
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

    let too_many_sources = session
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::ManyToOne,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![fixture.origin; 33],
            targets: vec![fixture.destination],
        })
        .await
        .unwrap_err();
    assert_contract_error(
        too_many_sources,
        ProviderError::InvalidRequest {
            reason: "matrix exceeds Via's source or target bound".into(),
        },
    );
    path
}

pub fn assert_contract_error(actual: ProviderError, expected: ProviderError) {
    assert_eq!(actual, expected);
}

pub fn assert_timeout(error: ProviderError) {
    assert_contract_error(error, ProviderError::Timeout);
}

pub fn assert_malformed_response(error: ProviderError) {
    assert_contract_error(error, ProviderError::MalformedResponse);
}

pub fn assert_unreachable(error: ProviderError) {
    assert_contract_error(error, ProviderError::UnreachableEndpoints);
}

pub fn assert_rate_limited(error: ProviderError) {
    assert_contract_error(error, ProviderError::RateLimited);
}

pub fn assert_revision_mismatch(error: ProviderError) {
    assert!(matches!(
        error,
        ProviderError::InconsistentGraphRevision { .. }
    ));
}

pub fn assert_cancellation_follow_up_succeeds<T>(result: Result<T, ProviderError>) {
    assert!(result.is_ok(), "cancellation poisoned the provider session");
}
