use std::sync::Arc;

mod support;

use via_server::street::{
    Coordinate, DetailedPathQuery, FakeProvider, GraphRevision, MatrixOrientation, MatrixQuery,
    MatrixReachability, Metres, ProviderError, Seconds, StreetRoutingProvider,
    StreetRoutingSession, StreetTravelMode,
};

use support::{
    ProviderContractFixture, assert_cancellation_follow_up_succeeds, assert_malformed_response,
    assert_provider_contract, assert_rate_limited, assert_revision_mismatch, assert_timeout,
    assert_unreachable,
};

fn taipei_main() -> Coordinate {
    Coordinate::try_new(25.04776, 121.51706).expect("fixture coordinate is valid")
}

fn taipei_101() -> Coordinate {
    Coordinate::try_new(25.03396, 121.56447).expect("fixture coordinate is valid")
}

fn daan_park() -> Coordinate {
    Coordinate::try_new(25.03344, 121.53409).expect("fixture coordinate is valid")
}

#[test]
fn coordinate_rejects_non_finite_and_out_of_range_values() {
    assert!(Coordinate::try_new(f64::NAN, 121.5).is_err());
    assert!(Coordinate::try_new(91.0, 121.5).is_err());
    assert!(Coordinate::try_new(25.0, 181.0).is_err());
}

#[tokio::test]
async fn fake_passes_the_shared_provider_contract_suite() {
    let path = via_server::street::DetailedPath {
        duration: Seconds(620),
        distance: Metres(3_900),
        geometry: vec![taipei_main(), taipei_101()],
        maneuvers: vec![
            via_server::street::Maneuver::depart(0),
            via_server::street::Maneuver::turn_right(0),
            via_server::street::Maneuver::new(via_server::street::ManeuverKind::UTurn, 1),
            via_server::street::Maneuver::arrive(1),
        ],
        graph_revision: GraphRevision::new("revision-a"),
        attribution: vec![],
    };
    let provider = FakeProvider::builder(GraphRevision::new("revision-a"))
        .matrix_leg(
            StreetTravelMode::Bicycle,
            taipei_main(),
            taipei_101(),
            Seconds(620),
            Metres(3_900),
        )
        .unreachable_leg(StreetTravelMode::Bicycle, taipei_main(), daan_park())
        .matrix_leg(
            StreetTravelMode::Walking,
            taipei_main(),
            taipei_101(),
            Seconds(1_200),
            Metres(4_100),
        )
        .matrix_leg(
            StreetTravelMode::Walking,
            daan_park(),
            taipei_101(),
            Seconds(600),
            Metres(2_000),
        )
        .path(StreetTravelMode::Bicycle, taipei_main(), taipei_101(), path)
        .build();
    let session = StreetRoutingSession::start(Arc::new(provider))
        .await
        .unwrap();

    assert_provider_contract(
        &session,
        &ProviderContractFixture {
            origin: taipei_main(),
            destination: taipei_101(),
            unreachable: daan_park(),
            alternate_origin: daan_park(),
            duration: Seconds(620),
            distance: Metres(3_900),
            geometry: vec![taipei_main(), taipei_101()],
            maneuver_kinds: vec![
                via_server::street::ManeuverKind::Depart,
                via_server::street::ManeuverKind::TurnRight,
                via_server::street::ManeuverKind::UTurn,
                via_server::street::ManeuverKind::Arrive,
            ],
        },
    )
    .await;
}

#[tokio::test]
async fn provider_contract_supports_one_to_many_and_unreachable_cells() {
    let provider = FakeProvider::builder(GraphRevision::new("taipei-2026-08-30"))
        .matrix_leg(
            StreetTravelMode::Walking,
            taipei_main(),
            taipei_101(),
            Seconds(1_200),
            Metres(1_450),
        )
        .unreachable_leg(StreetTravelMode::Walking, taipei_main(), daan_park())
        .build();
    let session = StreetRoutingSession::start(Arc::new(provider))
        .await
        .expect("provider is healthy and capable");

    let result = session
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![taipei_main()],
            targets: vec![taipei_101(), daan_park()],
        })
        .await
        .expect("matrix succeeds");

    assert_eq!(
        result.graph_revision,
        GraphRevision::new("taipei-2026-08-30")
    );
    assert_eq!(result.cells.len(), 1);
    assert_eq!(result.cells[0].len(), 2);
    assert_eq!(
        result.cells[0][0].reachability,
        MatrixReachability::Reachable {
            duration: Seconds(1_200),
            distance: Metres(1_450),
        }
    );
    assert_eq!(
        result.cells[0][1].reachability,
        MatrixReachability::Unreachable
    );
}

#[tokio::test]
async fn provider_contract_supports_many_to_one_youbike_matrices() {
    let provider = FakeProvider::builder(GraphRevision::new("taipei-2026-08-30"))
        .matrix_leg(
            StreetTravelMode::Bicycle,
            taipei_main(),
            taipei_101(),
            Seconds(620),
            Metres(3_900),
        )
        .matrix_leg(
            StreetTravelMode::Bicycle,
            daan_park(),
            taipei_101(),
            Seconds(310),
            Metres(1_900),
        )
        .build();
    let session = StreetRoutingSession::start(Arc::new(provider))
        .await
        .expect("provider is healthy and capable");

    let result = session
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::ManyToOne,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Bicycle,
            sources: vec![taipei_main(), daan_park()],
            targets: vec![taipei_101()],
        })
        .await
        .expect("matrix succeeds");

    assert_eq!(result.cells.len(), 2);
    assert_eq!(
        result.cells[1][0].reachability,
        MatrixReachability::Reachable {
            duration: Seconds(310),
            distance: Metres(1_900),
        }
    );
}

#[tokio::test]
async fn provider_contract_returns_provider_neutral_geometry_and_maneuvers() {
    let path = via_server::street::DetailedPath {
        duration: Seconds(620),
        distance: Metres(3_900),
        geometry: vec![taipei_main(), daan_park(), taipei_101()],
        maneuvers: vec![
            via_server::street::Maneuver::depart(0),
            via_server::street::Maneuver::turn_right(1),
            via_server::street::Maneuver::arrive(2),
        ],
        graph_revision: GraphRevision::new("taipei-2026-08-30"),
        attribution: vec![],
    };
    let provider = FakeProvider::builder(GraphRevision::new("taipei-2026-08-30"))
        .path(
            StreetTravelMode::Bicycle,
            taipei_main(),
            taipei_101(),
            path.clone(),
        )
        .build();
    let session = StreetRoutingSession::start(Arc::new(provider))
        .await
        .expect("provider is healthy and capable");

    let actual = session
        .detailed_path(DetailedPathQuery {
            travel_mode: StreetTravelMode::Bicycle,
            constraints: Default::default(),
            origin: taipei_main(),
            destination: taipei_101(),
        })
        .await
        .expect("path succeeds");

    assert_eq!(actual, path);
}

#[tokio::test]
async fn a_session_never_observes_a_replaced_graph_revision() {
    let provider = Arc::new(FakeProvider::builder(GraphRevision::new("revision-a")).build());
    let session = StreetRoutingSession::start(provider.clone())
        .await
        .expect("provider is initially healthy");
    provider
        .publish_revision(GraphRevision::new("revision-b"))
        .await;

    let error = session
        .correlate(vec![taipei_main()])
        .await
        .expect_err("revision replacement cannot leak into an active request");

    assert_revision_mismatch(error);
}

#[tokio::test]
async fn matrix_orientation_is_validated_before_calling_a_provider() {
    let provider: Arc<dyn StreetRoutingProvider> =
        Arc::new(FakeProvider::builder(GraphRevision::new("taipei-2026-08-30")).build());
    let session = StreetRoutingSession::start(provider)
        .await
        .expect("provider is healthy and capable");

    let error = session
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![taipei_main(), daan_park()],
            targets: vec![taipei_101()],
        })
        .await
        .expect_err("one-to-many has exactly one source");

    assert!(matches!(error, ProviderError::InvalidRequest { .. }));
}

#[tokio::test]
async fn deterministic_fake_can_program_provider_wide_failures() {
    let provider = FakeProvider::builder(GraphRevision::new("revision-a"))
        .matrix_failure(ProviderError::Timeout)
        .path_failure(ProviderError::MalformedResponse)
        .build();
    let session = StreetRoutingSession::start(Arc::new(provider))
        .await
        .unwrap();

    let matrix_error = session
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![taipei_main()],
            targets: vec![taipei_101()],
        })
        .await
        .unwrap_err();
    let path_error = session
        .detailed_path(DetailedPathQuery {
            travel_mode: StreetTravelMode::Walking,
            constraints: Default::default(),
            origin: taipei_main(),
            destination: taipei_101(),
        })
        .await
        .unwrap_err();

    assert_timeout(matrix_error);
    assert_malformed_response(path_error);
}

#[tokio::test]
async fn fake_passes_shared_error_translation_and_cancellation_checks() {
    let rate_limited = FakeProvider::builder(GraphRevision::new("revision-a"))
        .matrix_failure(ProviderError::RateLimited)
        .build();
    let rate_limited_session = StreetRoutingSession::start(Arc::new(rate_limited))
        .await
        .unwrap();
    let error = rate_limited_session
        .matrix(MatrixQuery {
            orientation: MatrixOrientation::OneToMany,
            constraints: Default::default(),
            travel_mode: StreetTravelMode::Walking,
            sources: vec![taipei_main()],
            targets: vec![taipei_101()],
        })
        .await
        .unwrap_err();
    assert_rate_limited(error);

    let provider = Arc::new(FakeProvider::builder(GraphRevision::new("revision-a")).build());
    let session = StreetRoutingSession::start(provider).await.unwrap();
    let unreachable = session
        .detailed_path(DetailedPathQuery {
            travel_mode: StreetTravelMode::Walking,
            constraints: Default::default(),
            origin: taipei_main(),
            destination: taipei_101(),
        })
        .await
        .unwrap_err();
    assert_unreachable(unreachable);

    let cancelled_session = session.clone();
    let cancelled =
        tokio::spawn(async move { cancelled_session.correlate(vec![taipei_main()]).await });
    cancelled.abort();
    assert_cancellation_follow_up_succeeds(session.correlate(vec![taipei_main()]).await);
}
