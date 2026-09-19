use std::sync::Arc;

use crate::support::routing::{
    algorithm_query as query, resolver, routing_network as network,
    routing_network_with_mode as network_with_mode, stop, trip,
};
use time::{Time, Weekday, macros::datetime};
use via_server::domain::ids::{BoardingPointId, TransitServiceId};
use via_server::domain::transit::{
    ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService,
};
use via_server::domain::transit_timing::{DepartureQuery, DepartureResolver, TimingBasis};
use via_server::providers::timing::{
    BusDepartureSource, DepartureDispatcher, TransitDepartureSource,
};
use via_server::routing::algorithm::RoutingAlgorithm;
use via_server::routing::transfers::TransferShortcutIndex;
use via_server::routing::trip_based::TripBasedRouter;
use via_server::routing::{AccessPoint, AlgorithmQuery, EgressPoint, JourneySegment};

#[tokio::test]
async fn scheduled_router_selects_the_earliest_boardable_trip() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![
        trip(
            "late",
            "line",
            &["a", "b"],
            &[900, 1_500],
            &[900, 1_500],
            false,
        ),
        trip(
            "early",
            "line",
            &["a", "b"],
            &[300, 900],
            &[300, 900],
            false,
        ),
    ]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router.route(query("a", "b")).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].arrival_time, datetime!(2026-08-31 10:15 UTC));
    assert!(matches!(
        &candidates[0].segments[0],
        JourneySegment::Transit { .. }
    ));
}

#[tokio::test]
async fn query_allowed_transit_modes_filter_services_before_departure_resolution() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![trip(
        "bus-trip",
        "line",
        &["a", "b"],
        &[300, 900],
        &[300, 900],
        false,
    )]);
    let calls = resolver.calls.clone();
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );

    let candidates = router
        .route(query("a", "b").with_allowed_transit_modes([TransitMode::Metro]))
        .await
        .unwrap();

    assert!(candidates.is_empty());
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn runtime_resolution_is_lazy_and_scans_the_effective_trip_forward() {
    let network = network(&[("explored", &["a", "b"]), ("unused", &["x", "y"])]);
    let resolver = resolver(vec![trip(
        "runtime-bus",
        "explored",
        &["a", "b"],
        &[300, 900],
        &[300, 900],
        true,
    )]);
    let calls = resolver.calls.clone();
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router.route(query("a", "b")).await.unwrap();
    assert_eq!(candidates[0].arrival_time, datetime!(2026-08-31 10:15 UTC));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        calls.lock().unwrap()[0].service_id,
        TransitServiceId::new("explored")
    );
}

#[tokio::test]
async fn a_later_candidate_can_win_when_the_earliest_candidate_is_not_feasible() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![
        trip(
            "too-late",
            "line",
            &["a", "b"],
            &[300, 3_000],
            &[300, 3_000],
            true,
        ),
        trip(
            "usable",
            "line",
            &["a", "b"],
            &[600, 1_800],
            &[600, 1_800],
            true,
        ),
    ]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router
        .route(query("a", "b").with_latest_arrival(datetime!(2026-08-31 10:30 UTC)))
        .await
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].arrival_time, datetime!(2026-08-31 10:30 UTC));
}

#[tokio::test]
async fn trip_reached_from_an_earlier_index_is_not_rescanned() {
    let network = network(&[("line", &["a", "b", "c"])]);
    let resolver = resolver(vec![trip(
        "trip",
        "line",
        &["a", "b", "c"],
        &[60, 300, 600],
        &[60, 300, 600],
        false,
    )]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let query = AlgorithmQuery::new(
        vec![
            AccessPoint::new(
                BoardingPointId::new("a"),
                datetime!(2026-08-31 10:00 UTC),
                datetime!(2026-08-31 10:00 UTC),
                0,
                0,
            ),
            AccessPoint::new(
                BoardingPointId::new("b"),
                datetime!(2026-08-31 10:00 UTC),
                datetime!(2026-08-31 10:00 UTC),
                30,
                0,
            ),
        ],
        vec![EgressPoint::new(BoardingPointId::new("c"), 0, 0)],
        datetime!(2026-08-31 10:00 UTC),
    );
    let candidates = router.route(query).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(matches!(
        candidates[0].segments[0],
        JourneySegment::Transit { board_index: 0, .. }
    ));
}

#[tokio::test]
async fn dominated_runtime_departure_is_removed() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![
        trip("early", "line", &["a", "b"], &[60, 600], &[60, 600], true),
        trip("late", "line", &["a", "b"], &[120, 900], &[120, 900], true),
    ]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let candidates = router.route(query("a", "b")).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].segments[0].service_id(),
        Some(&TransitServiceId::new("line"))
    );
}

#[tokio::test]
async fn duplicate_timing_needs_are_coalesced() {
    let network = network(&[("line", &["a", "b"])]);
    let resolver = resolver(vec![trip(
        "trip",
        "line",
        &["a", "b"],
        &[60, 600],
        &[60, 600],
        true,
    )]);
    let calls = resolver.calls.clone();
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let base = AccessPoint::new(
        BoardingPointId::new("a"),
        datetime!(2026-08-31 10:00 UTC),
        datetime!(2026-08-31 10:00 UTC),
        0,
        0,
    );
    let query = AlgorithmQuery::new(
        vec![base.clone(), base],
        vec![EgressPoint::new(BoardingPointId::new("b"), 0, 0)],
        datetime!(2026-08-31 10:00 UTC),
    );
    router.route(query).await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn departure_resolver_falls_back_to_schedule() {
    let stops = vec![stop("a"), stop("b")];
    let services = vec![TransitService::new(
        TransitServiceId::new("line"),
        TransitMode::Bus,
        "line",
        "b",
        vec![BoardingPointId::new("a"), BoardingPointId::new("b")],
    )];
    let schedule = Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        stops,
        services,
        vec![ScheduledTrip::weekdays(
            via_server::domain::ids::ScheduledTripId::new("run"),
            TransitServiceId::new("line"),
            vec![Weekday::Monday],
            vec![
                ScheduledStopTime::new(36_060, 36_060),
                ScheduledStopTime::new(36_120, 36_120),
            ],
        )],
    ));
    let bus = Arc::new(BusDepartureSource::new(schedule.clone(), None));
    let resolver = DepartureDispatcher::new(schedule, vec![bus as Arc<dyn TransitDepartureSource>]);
    let query = DepartureQuery::new(
        TransitServiceId::new("line"),
        BoardingPointId::new("a"),
        datetime!(2026-08-31 02:00 UTC),
        1,
    );
    let departures = resolver.next_departures(query).await.unwrap();
    assert_eq!(departures.len(), 1);
    assert!(matches!(departures[0].timing_basis, TimingBasis::Schedule));
}

#[tokio::test]
async fn schedule_departures_use_indexed_service_day_order_and_exceptions() {
    let board = stop("a");
    let destination = stop("b");
    let service_id = TransitServiceId::new("line");
    let services = vec![TransitService::new(
        service_id.clone(),
        TransitMode::Bus,
        "line",
        "b",
        vec![board.id.clone(), destination.id.clone()],
    )];
    let monday = time::macros::date!(2026 - 08 - 31);
    let network = Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        vec![board, destination],
        services,
        vec![
            ScheduledTrip::weekdays(
                via_server::domain::ids::ScheduledTripId::new("weekday"),
                service_id.clone(),
                vec![Weekday::Monday],
                vec![
                    ScheduledStopTime::new(36_600, 36_600),
                    ScheduledStopTime::new(37_200, 37_200),
                ],
            ),
            ScheduledTrip::weekdays(
                via_server::domain::ids::ScheduledTripId::new("added"),
                service_id.clone(),
                vec![Weekday::Tuesday],
                vec![
                    ScheduledStopTime::new(36_900, 36_900),
                    ScheduledStopTime::new(37_500, 37_500),
                ],
            )
            .with_service_day_exceptions(vec![monday], Vec::new()),
            ScheduledTrip::weekdays(
                via_server::domain::ids::ScheduledTripId::new("removed"),
                service_id.clone(),
                vec![Weekday::Monday],
                vec![
                    ScheduledStopTime::new(36_300, 36_300),
                    ScheduledStopTime::new(36_900, 36_900),
                ],
            )
            .with_service_day_exceptions(Vec::new(), vec![monday]),
            ScheduledTrip::weekdays(
                via_server::domain::ids::ScheduledTripId::new("after-midnight"),
                service_id.clone(),
                vec![Weekday::Monday],
                vec![
                    ScheduledStopTime::new(90_000, 90_000),
                    ScheduledStopTime::new(90_600, 90_600),
                ],
            ),
        ],
    ));
    let resolver = via_server::domain::transit_timing::ScheduleDepartureResolver::new(network);

    let departures = resolver
        .next_departures(DepartureQuery::new(
            service_id.clone(),
            BoardingPointId::new("a"),
            datetime!(2026-08-31 10:05 +08:00),
            3,
        ))
        .await
        .unwrap();
    assert_eq!(
        departures
            .iter()
            .map(|trip| trip.scheduled_trip_id.as_ref().unwrap().as_str())
            .collect::<Vec<_>>(),
        vec!["weekday", "added", "after-midnight"]
    );

    let after_midnight = resolver
        .next_departures(DepartureQuery::new(
            service_id.clone(),
            BoardingPointId::new("a"),
            datetime!(2026-09-01 00:55 +08:00),
            1,
        ))
        .await
        .unwrap();
    assert_eq!(
        after_midnight[0]
            .scheduled_trip_id
            .as_ref()
            .unwrap()
            .as_str(),
        "after-midnight"
    );

    let no_match = resolver
        .next_departures(DepartureQuery::new(
            service_id,
            BoardingPointId::new("a"),
            datetime!(2026-09-02 05:00 +08:00),
            1,
        ))
        .await
        .unwrap();
    assert!(no_match.is_empty());
}

#[tokio::test]
async fn algorithm_behavior_is_independent_of_transit_mode() {
    let resolver = resolver(vec![trip(
        "trip",
        "line",
        &["a", "b"],
        &[60, 600],
        &[60, 600],
        false,
    )]);
    let bus_router = TripBasedRouter::new(
        network(&[("line", &["a", "b"])]),
        Arc::new(resolver.clone()),
        Arc::new(TransferShortcutIndex::default()),
    );
    let metro_router = TripBasedRouter::new(
        network_with_mode(&[("line", &["a", "b"])], TransitMode::Metro),
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );
    let (bus, metro) = tokio::join!(
        bus_router.route(query("a", "b")),
        metro_router.route(query("a", "b"))
    );
    assert_eq!(bus.unwrap()[0].signature(), metro.unwrap()[0].signature());
}
