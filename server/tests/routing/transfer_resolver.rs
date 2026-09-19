use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::{Duration, Time, Weekday, macros::datetime};
use via_server::{
    api::RequestCoordinate,
    domain::{
        ids::{BoardingPointId, ScheduledTripId, TransitServiceId},
        transit::{
            ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork, TransitService,
            TransitStop,
        },
        transit_timing::ScheduleDepartureResolver,
    },
    routing::{
        AccessPoint, AlgorithmQuery, EgressPoint, JourneySegment, ResolvedTransfer,
        RoutingAlgorithm, TransferNeed, TransferResolver, TransferShortcut, TransferTarget,
        trip_based::TripBasedRouter,
    },
};

#[derive(Default)]
struct FakeTransferResolver {
    batches: Mutex<Vec<Vec<TransferNeed>>>,
    allow_b_to_c: bool,
}

#[async_trait]
impl TransferResolver for FakeTransferResolver {
    async fn expand_batch(&self, needs: &[TransferNeed]) -> Vec<ResolvedTransfer> {
        self.batches.lock().unwrap().push(needs.to_vec());
        needs
            .iter()
            .enumerate()
            .filter(|(_, need)| self.allow_b_to_c && need.source == BoardingPointId::new("b"))
            .filter_map(|(input_index, need)| {
                ResolvedTransfer::new(
                    input_index,
                    need,
                    TransferTarget::BoardingPoint(BoardingPointId::new("c")),
                    need.ready_at + Duration::seconds(40),
                    JourneySegment::WalkingTransfer {
                        shortcut: TransferShortcut::new(
                            BoardingPointId::new("b"),
                            BoardingPointId::new("c"),
                            40,
                        ),
                    },
                    40,
                )
            })
            .collect()
    }
}

fn network(second_start: &str) -> Arc<TransitNetwork> {
    let ids = ["a", "b", "x", "c", "d"];
    let stops = ids
        .into_iter()
        .map(|id| {
            TransitStop::new(
                BoardingPointId::new(id),
                id,
                RequestCoordinate {
                    latitude: 25.0,
                    longitude: 121.5,
                },
            )
        })
        .collect();
    let second_pattern = [second_start, "d"];
    let services = [
        ("first", &["a", "b", "x"][..]),
        ("second", &second_pattern[..]),
    ]
    .into_iter()
    .map(|(id, pattern)| {
        TransitService::new(
            TransitServiceId::new(id),
            TransitMode::Bus,
            id,
            "destination",
            pattern
                .iter()
                .map(|stop| BoardingPointId::new(*stop))
                .collect(),
        )
    })
    .collect();
    let trips = [
        (
            "first",
            &[10 * 3600 + 60, 10 * 3600 + 300, 10 * 3600 + 360][..],
        ),
        ("second", &[10 * 3600 + 540, 10 * 3600 + 900][..]),
    ]
    .into_iter()
    .map(|(id, times)| {
        ScheduledTrip::weekdays(
            ScheduledTripId::new(format!("{id}-trip")),
            TransitServiceId::new(id),
            vec![Weekday::Monday],
            times
                .iter()
                .map(|seconds| ScheduledStopTime::new(*seconds, *seconds))
                .collect(),
        )
    })
    .collect();
    Arc::new(TransitNetwork::new(
        Time::from_hms(4, 0, 0).unwrap(),
        stops,
        services,
        trips,
    ))
}

#[tokio::test]
async fn batched_fake_transfer_reaches_the_next_transit_service() {
    let network = network("c");
    let transfers = Arc::new(FakeTransferResolver {
        allow_b_to_c: true,
        ..Default::default()
    });
    let router = TripBasedRouter::new(
        network.clone(),
        Arc::new(ScheduleDepartureResolver::new(network)),
        transfers.clone(),
    );
    let departure = datetime!(2026-08-31 10:00 +08:00);
    let query = AlgorithmQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            departure,
            departure,
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("d"), 0, 0)],
        departure,
    )
    .with_limits(1, 16, 2_000, 3, 4 * 3600);
    let candidates = router.route(query).await.unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(
        candidate.transit_service_sequence(),
        vec![
            TransitServiceId::new("first"),
            TransitServiceId::new("second"),
        ]
    );
    assert_eq!(candidate.transfer_count, 1);
    assert_eq!(candidate.walking_seconds, 40);
    assert_eq!(candidate.arrival_time, datetime!(2026-08-31 10:15 +08:00));
    assert!(matches!(
        candidate.segments.as_slice(),
        [
            JourneySegment::Transit { .. },
            JourneySegment::WalkingTransfer { .. },
            JourneySegment::Transit { .. }
        ]
    ));
    let batches = transfers.batches.lock().unwrap();
    assert_eq!(batches.len(), 2);
    assert_eq!(
        batches[0]
            .iter()
            .map(|need| need.source.as_str())
            .collect::<Vec<_>>(),
        vec!["a"]
    );
    assert_eq!(
        batches[1]
            .iter()
            .map(|need| need.source.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "x"]
    );
}

#[tokio::test]
async fn same_stop_interchange_succeeds_with_an_empty_transfer_resolver() {
    let network = network("x");
    let transfers = Arc::new(FakeTransferResolver::default());
    let router = TripBasedRouter::new(
        network.clone(),
        Arc::new(ScheduleDepartureResolver::new(network)),
        transfers.clone(),
    );
    let departure = datetime!(2026-08-31 10:00 +08:00);
    let query = AlgorithmQuery::new(
        vec![AccessPoint::new(
            BoardingPointId::new("a"),
            departure,
            departure,
            0,
            0,
        )],
        vec![EgressPoint::new(BoardingPointId::new("d"), 0, 0)],
        departure,
    )
    .with_limits(1, 16, 2_000, 3, 4 * 3600);
    let candidates = router.route(query).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].transfer_count, 1);
    assert_eq!(candidates[0].walking_seconds, 0);
    assert_eq!(
        candidates[0].transit_service_sequence(),
        vec![
            TransitServiceId::new("first"),
            TransitServiceId::new("second"),
        ]
    );
    assert!(
        candidates[0]
            .segments
            .iter()
            .all(|segment| matches!(segment, JourneySegment::Transit { .. }))
    );
    let batches = transfers.batches.lock().unwrap();
    assert_eq!(batches.len(), 2);
    assert!(
        batches[1]
            .iter()
            .any(|need| need.source == BoardingPointId::new("x"))
    );
}

#[tokio::test]
async fn resolver_batches_are_bounded_for_many_distinct_access_points() {
    let network = network("c");
    let transfers = Arc::new(FakeTransferResolver::default());
    let router = TripBasedRouter::new(
        network.clone(),
        Arc::new(ScheduleDepartureResolver::new(network)),
        transfers.clone(),
    );
    let departure = datetime!(2026-08-31 10:00 +08:00);
    let access = (0..130)
        .map(|index| {
            AccessPoint::new(
                BoardingPointId::new(format!("unserved-{index}")),
                departure,
                departure,
                0,
                0,
            )
        })
        .collect();
    let query = AlgorithmQuery::new(
        access,
        vec![EgressPoint::new(BoardingPointId::new("d"), 0, 0)],
        departure,
    );
    assert!(router.route(query).await.unwrap().is_empty());
    let batches = transfers.batches.lock().unwrap();
    assert_eq!(
        batches.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![128, 2]
    );
}
