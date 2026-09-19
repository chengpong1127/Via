use std::collections::HashMap;
use std::sync::Arc;

use via_server::domain::ids::BoardingPointId;
use via_server::routing::transfers::{TransferShortcut, TransferShortcutIndex};
use via_server::routing::trip_based::TripBasedRouter;
use via_server::routing::{JourneySegment, RoutingAlgorithm, TransferIndex};

use crate::support::routing::{
    FixtureResolver, algorithm_query as query, resolver, routing_network as network, trip,
};

#[tokio::test]
async fn walking_transfer_shortcut_expands_a_second_service() {
    let network = network(&[("first", &["a", "b"]), ("second", &["c", "d"])]);
    let resolver = FixtureResolver {
        trips: Arc::new(HashMap::from([
            (
                via_server::domain::ids::TransitServiceId::new("first"),
                vec![trip(
                    "first-trip",
                    "first",
                    &["a", "b"],
                    &[60, 600],
                    &[60, 600],
                    false,
                )],
            ),
            (
                via_server::domain::ids::TransitServiceId::new("second"),
                vec![trip(
                    "second-trip",
                    "second",
                    &["c", "d"],
                    &[1_200, 1_800],
                    &[1_200, 1_800],
                    false,
                )],
            ),
        ])),
        ..Default::default()
    };
    let transfer = TransferShortcutIndex::new([TransferShortcut::new(
        BoardingPointId::new("b"),
        BoardingPointId::new("c"),
        60,
    )]);
    let router = TripBasedRouter::new(network, Arc::new(resolver), Arc::new(transfer));
    let candidates = router.route(query("a", "d")).await.unwrap();
    assert!(candidates.iter().any(|candidate| {
        candidate.transfer_count == 1
            && candidate
                .segments
                .iter()
                .filter(|segment| matches!(segment, JourneySegment::Transit { .. }))
                .count()
                == 2
    }));
}

#[tokio::test]
async fn same_stop_transfer_uses_default_transfer_seconds() {
    let network = network(&[("first", &["a", "x"]), ("second", &["x", "d"])]);
    let resolver = FixtureResolver {
        trips: Arc::new(HashMap::from([
            (
                via_server::domain::ids::TransitServiceId::new("first"),
                vec![trip(
                    "first-trip",
                    "first",
                    &["a", "x"],
                    &[60, 300],
                    &[60, 300],
                    false,
                )],
            ),
            (
                via_server::domain::ids::TransitServiceId::new("second"),
                vec![trip(
                    "second-trip",
                    "second",
                    &["x", "d"],
                    &[420, 900],
                    &[420, 900],
                    false,
                )],
            ),
        ])),
        ..Default::default()
    };
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );

    let candidates = router.route(query("a", "d")).await.unwrap();

    assert!(candidates.iter().any(|candidate| {
        candidate.transit_service_sequence()
            == vec![
                via_server::domain::ids::TransitServiceId::new("first"),
                via_server::domain::ids::TransitServiceId::new("second"),
            ]
            && candidate.transfer_count == 1
    }));
}

#[tokio::test]
async fn same_stop_transfer_is_not_boardable_before_default_transfer_time() {
    let network = network(&[("first", &["a", "x"]), ("second", &["x", "d"])]);
    let resolver = FixtureResolver {
        trips: Arc::new(HashMap::from([
            (
                via_server::domain::ids::TransitServiceId::new("first"),
                vec![trip(
                    "first-trip",
                    "first",
                    &["a", "x"],
                    &[60, 300],
                    &[60, 300],
                    false,
                )],
            ),
            (
                via_server::domain::ids::TransitServiceId::new("second"),
                vec![trip(
                    "second-trip",
                    "second",
                    &["x", "d"],
                    &[359, 900],
                    &[359, 900],
                    false,
                )],
            ),
        ])),
        ..Default::default()
    };
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );

    assert!(router.route(query("a", "d")).await.unwrap().is_empty());
}

#[tokio::test]
async fn same_service_does_not_create_a_pointless_transfer_round() {
    let network = network(&[("line", &["a", "x", "d"])]);
    let resolver = resolver(vec![trip(
        "line-trip",
        "line",
        &["a", "x", "d"],
        &[60, 300, 600],
        &[60, 300, 600],
        false,
    )]);
    let router = TripBasedRouter::new(
        network,
        Arc::new(resolver),
        Arc::new(TransferShortcutIndex::default()),
    );

    let candidates = router.route(query("a", "d")).await.unwrap();

    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.transfer_count == 0)
    );
}

#[tokio::test]
async fn same_stop_and_walking_transfers_compete() {
    let network = network(&[
        ("first", &["a", "x"]),
        ("same-stop", &["x", "d"]),
        ("walking", &["y", "d"]),
    ]);
    let resolver = FixtureResolver {
        trips: Arc::new(HashMap::from([
            (
                via_server::domain::ids::TransitServiceId::new("first"),
                vec![trip(
                    "first-trip",
                    "first",
                    &["a", "x"],
                    &[60, 300],
                    &[60, 300],
                    false,
                )],
            ),
            (
                via_server::domain::ids::TransitServiceId::new("same-stop"),
                vec![trip(
                    "same-stop-trip",
                    "same-stop",
                    &["x", "d"],
                    &[420, 900],
                    &[420, 900],
                    false,
                )],
            ),
            (
                via_server::domain::ids::TransitServiceId::new("walking"),
                vec![trip(
                    "walking-trip",
                    "walking",
                    &["y", "d"],
                    &[390, 800],
                    &[390, 800],
                    false,
                )],
            ),
        ])),
        ..Default::default()
    };
    let transfer = TransferShortcutIndex::new([TransferShortcut::new(
        BoardingPointId::new("x"),
        BoardingPointId::new("y"),
        60,
    )]);
    let router = TripBasedRouter::new(network, Arc::new(resolver), Arc::new(transfer));

    let candidates = router.route(query("a", "d")).await.unwrap();
    let sequences = candidates
        .iter()
        .map(|candidate| candidate.transit_service_sequence())
        .collect::<Vec<_>>();

    assert!(sequences.contains(&vec![
        via_server::domain::ids::TransitServiceId::new("first"),
        via_server::domain::ids::TransitServiceId::new("same-stop"),
    ]));
    assert!(sequences.contains(&vec![
        via_server::domain::ids::TransitServiceId::new("first"),
        via_server::domain::ids::TransitServiceId::new("walking"),
    ]));
}

#[tokio::test]
async fn pareto_keeps_earlier_extra_transfer_and_later_direct_result() {
    let network = network(&[
        ("first", &["a", "b"]),
        ("second", &["c", "d"]),
        ("direct", &["a", "d"]),
    ]);
    let resolver = FixtureResolver {
        trips: Arc::new(HashMap::from([
            (
                via_server::domain::ids::TransitServiceId::new("first"),
                vec![trip(
                    "first-trip",
                    "first",
                    &["a", "b"],
                    &[60, 300],
                    &[60, 300],
                    false,
                )],
            ),
            (
                via_server::domain::ids::TransitServiceId::new("second"),
                vec![trip(
                    "second-trip",
                    "second",
                    &["c", "d"],
                    &[600, 900],
                    &[600, 900],
                    false,
                )],
            ),
            (
                via_server::domain::ids::TransitServiceId::new("direct"),
                vec![trip(
                    "direct-trip",
                    "direct",
                    &["a", "d"],
                    &[120, 1_500],
                    &[120, 1_500],
                    false,
                )],
            ),
        ])),
        ..Default::default()
    };
    let transfer = TransferShortcutIndex::new([TransferShortcut::new(
        BoardingPointId::new("b"),
        BoardingPointId::new("c"),
        60,
    )]);
    let router = TripBasedRouter::new(network, Arc::new(resolver), Arc::new(transfer));
    let candidates = router.route(query("a", "d")).await.unwrap();
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.transfer_count == 1)
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.transfer_count == 0)
    );
}

#[test]
fn transfer_index_preserves_multiple_nondominated_alternatives() {
    let index = TransferShortcutIndex::new([
        TransferShortcut::new(BoardingPointId::new("b"), BoardingPointId::new("c"), 120),
        TransferShortcut::new(BoardingPointId::new("b"), BoardingPointId::new("c"), 180),
    ]);
    assert_eq!(index.transfers_from(&BoardingPointId::new("b")).len(), 2);
}
