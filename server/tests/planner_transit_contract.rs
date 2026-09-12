use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::macros::{date, datetime};
use tower::ServiceExt;
use via_server::{
    api::RequestCoordinate,
    app::build_router_with_services_and_network,
    live_status::{Freshness, FreshnessState},
    normalization::{
        ActionState, ArrivalEstimate, ArrivalState, BikeAvailability, BikeShareStationId,
        BoardingPointId, ScheduledRunId, TransitServiceId,
    },
    snapshot::{InMemorySnapshotStore, LiveStatusValue, NetworkSnapshot, NormalizedLiveStatus},
    street::{
        Coordinate, DetailedPath, FakeProvider, GraphRevision, Maneuver, Metres, ProviderError,
        Seconds, StreetTravelMode,
    },
    transit::{
        BikeShareStation, ScheduledStopTime, ScheduledTrip, TransitMode, TransitNetwork,
        TransitService, TransitStop,
    },
};

fn coordinate(latitude: f64, longitude: f64) -> Coordinate {
    Coordinate::try_new(latitude, longitude).unwrap()
}

fn path(revision: GraphRevision, from: Coordinate, to: Coordinate, duration: u32) -> DetailedPath {
    DetailedPath {
        duration: Seconds(duration),
        distance: Metres(duration * 5),
        geometry: vec![from, to],
        maneuvers: vec![Maneuver::depart(0), Maneuver::arrive(1)],
        graph_revision: revision,
        attribution: Vec::new(),
    }
}

fn request() -> Value {
    json!({
        "origin": {
            "coordinate": {"latitude": 25.000, "longitude": 121.500},
            "display_text": "Origin"
        },
        "destination": {
            "coordinate": {"latitude": 25.020, "longitude": 121.520},
            "display_text": "Destination"
        },
        "selected_modes": ["transit"],
        "locale": "en",
        "departure_setting": {"type": "depart_at", "time": "2026-08-31T10:00:00+08:00"}
    })
}

fn network() -> TransitNetwork {
    let first = BoardingPointId::new("via_boarding_1");
    let second = BoardingPointId::new("via_boarding_2");
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![
            TransitStop::new(
                first.clone(),
                "First Station",
                RequestCoordinate {
                    latitude: 25.010,
                    longitude: 121.510,
                },
            ),
            TransitStop::new(
                second.clone(),
                "Second Station",
                RequestCoordinate {
                    latitude: 25.015,
                    longitude: 121.515,
                },
            ),
        ],
        vec![TransitService::new(
            TransitServiceId::new("via_metro_blue"),
            TransitMode::Metro,
            "Blue Line",
            "Second Station",
            vec![first, second],
        )],
        vec![ScheduledTrip::weekdays(
            ScheduledRunId::new("via_run_1"),
            TransitServiceId::new("via_metro_blue"),
            vec![time::Weekday::Monday],
            vec![
                ScheduledStopTime::new(10 * 3600 + 600, 10 * 3600 + 600),
                ScheduledStopTime::new(10 * 3600 + 1_200, 10 * 3600 + 1_200),
            ],
        )],
        Vec::new(),
    )
}

fn bus_network() -> TransitNetwork {
    let first = BoardingPointId::new("via_bus_boarding_1");
    let second = BoardingPointId::new("via_bus_boarding_2");
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![
            TransitStop::new(
                first.clone(),
                "First Bus Stop",
                RequestCoordinate {
                    latitude: 25.010,
                    longitude: 121.510,
                },
            ),
            TransitStop::new(
                second.clone(),
                "Second Bus Stop",
                RequestCoordinate {
                    latitude: 25.015,
                    longitude: 121.515,
                },
            ),
        ],
        vec![TransitService::new(
            TransitServiceId::new("via_bus_1"),
            TransitMode::Bus,
            "Bus 1",
            "Second Bus Stop",
            vec![first, second],
        )],
        vec![ScheduledTrip::weekdays(
            ScheduledRunId::new("via_bus_run_1"),
            TransitServiceId::new("via_bus_1"),
            vec![time::Weekday::Monday],
            vec![
                ScheduledStopTime::new(10 * 3600 + 600, 10 * 3600 + 600),
                ScheduledStopTime::new(10 * 3600 + 1_200, 10 * 3600 + 1_200),
            ],
        )],
        Vec::new(),
    )
}

#[tokio::test]
async fn transit_candidate_uses_access_timetable_egress_and_schedule_class() {
    let revision = GraphRevision::new("revision-a");
    let origin = coordinate(25.000, 121.500);
    let first = coordinate(25.010, 121.510);
    let second = coordinate(25.015, 121.515);
    let destination = coordinate(25.020, 121.520);
    let provider = FakeProvider::builder(revision.clone())
        .path(
            StreetTravelMode::Walking,
            origin,
            first,
            path(revision.clone(), origin, first, 120),
        )
        .path(
            StreetTravelMode::Walking,
            second,
            destination,
            path(revision.clone(), second, destination, 120),
        )
        .build();
    let now = datetime!(2026-08-31 09:59 +08:00);
    let snapshots = Arc::new(InMemorySnapshotStore::new(NetworkSnapshot {
        id: "snapshot-transit".into(),
        created_at: now,
        live_cutoff: now,
        static_network_revision: "static-1".into(),
        schedule_revision: "schedule-1".into(),
        freshness_policy_version: "1".into(),
        publication_version: 1,
        feed_evidence: Vec::new(),
        live_status: Vec::new(),
    }));
    let app = build_router_with_services_and_network(
        Arc::new(provider),
        snapshots,
        None,
        None,
        network(),
    )
    .await
    .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(request().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();

    assert_eq!(body["journey_options"].as_array().unwrap().len(), 1);
    let option = &body["journey_options"][0];
    assert_eq!(option["class"], "feasible");
    assert_eq!(option["travel_modes"], json!(["walking", "transit"]));
    assert_eq!(option["legs"][1]["mode"], "transit");
    assert_eq!(option["legs"][1]["details"]["type"], "transit");
    assert_eq!(option["legs"][1]["details"]["transit_mode"], "metro");
    assert!(option["id"].as_str().unwrap().len() > "option_1".len());
    assert_ne!(option["legs"][0]["id"], "leg_1");
    assert_eq!(body["effective_search_time"], "2026-08-31T10:00:00+08:00");
    assert_eq!(body["time_zone"], "Asia/Taipei");
    assert_eq!(body["network_snapshot"]["id"], "snapshot-transit");
    assert_eq!(date!(2026 - 08 - 31).weekday(), time::Weekday::Monday);
}

#[tokio::test]
async fn bus_candidate_uses_fresh_arrival_estimate_and_delay() {
    let revision = GraphRevision::new("revision-a");
    let origin = coordinate(25.000, 121.500);
    let first = coordinate(25.010, 121.510);
    let second = coordinate(25.015, 121.515);
    let destination = coordinate(25.020, 121.520);
    let provider = FakeProvider::builder(revision.clone())
        .path(
            StreetTravelMode::Walking,
            origin,
            first,
            path(revision.clone(), origin, first, 120),
        )
        .path(
            StreetTravelMode::Walking,
            second,
            destination,
            path(revision.clone(), second, destination, 120),
        )
        .build();
    let now = datetime!(2026-08-31 10:00 +08:00);
    let snapshot = NetworkSnapshot {
        id: "snapshot-bus".into(),
        created_at: now,
        live_cutoff: now,
        static_network_revision: "static-1".into(),
        schedule_revision: "schedule-1".into(),
        freshness_policy_version: "1".into(),
        publication_version: 1,
        feed_evidence: Vec::new(),
        live_status: vec![NormalizedLiveStatus {
            id: "bus-arrival".into(),
            capability: "bus_arrival_taipei".into(),
            received_at: now,
            normalized_at: now,
            freshness: Freshness {
                state: FreshnessState::Fresh,
                reason: None,
                policy_version: "1".into(),
                evaluated_at: now,
            },
            value: LiveStatusValue::ArrivalEstimate(ArrivalEstimate {
                transit_service_id: TransitServiceId::new("via_bus_1"),
                boarding_point_id: BoardingPointId::new("via_bus_boarding_1"),
                predicted_arrival_at: Some(datetime!(2026-08-31 10:12 +08:00)),
                state: ArrivalState::Predicted,
                transmitted_at: now,
            }),
        }],
    };
    let app = build_router_with_services_and_network(
        Arc::new(provider),
        Arc::new(InMemorySnapshotStore::new(snapshot)),
        None,
        None,
        bus_network(),
    )
    .await
    .unwrap();
    let mut bus_request = request();
    bus_request["selected_modes"] = json!(["transit"]);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(bus_request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();

    let option = &body["journey_options"][0];
    assert_eq!(option["class"], "reliable");
    assert_eq!(option["legs"][1]["mode"], "transit");
    assert_eq!(option["legs"][1]["details"]["transit_mode"], "bus");
    assert_eq!(
        option["legs"][1]["departure_time"],
        "2026-08-31T10:12:00+08:00"
    );
    assert_eq!(
        option["legs"][1]["live_status"][0]["kind"],
        "arrival_estimate"
    );
}

#[tokio::test]
async fn youbike_candidate_uses_actual_fresh_station_counts() {
    let revision = GraphRevision::new("revision-a");
    let origin = coordinate(25.000, 121.500);
    let pickup = coordinate(25.005, 121.505);
    let dropoff = coordinate(25.010, 121.510);
    let destination = coordinate(25.020, 121.520);
    let provider = FakeProvider::builder(revision.clone())
        .path(
            StreetTravelMode::Walking,
            origin,
            pickup,
            path(revision.clone(), origin, pickup, 60),
        )
        .path(
            StreetTravelMode::Bicycle,
            pickup,
            dropoff,
            path(revision.clone(), pickup, dropoff, 600),
        )
        .path(
            StreetTravelMode::Walking,
            dropoff,
            destination,
            path(revision.clone(), dropoff, destination, 60),
        )
        .build();
    let now = datetime!(2026-08-31 10:00 +08:00);
    let snapshot = NetworkSnapshot {
        id: "snapshot-bike".into(),
        created_at: now,
        live_cutoff: now,
        static_network_revision: "static-1".into(),
        schedule_revision: "schedule-1".into(),
        freshness_policy_version: "1".into(),
        publication_version: 1,
        feed_evidence: Vec::new(),
        live_status: vec![
            bike_status("bike-pickup", "station-pickup", 2, 4, now),
            bike_status("bike-dropoff", "station-dropoff", 4, 2, now),
        ],
    };
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![
            BikeShareStation::new(
                BikeShareStationId::new("station-pickup"),
                "Pickup Station",
                RequestCoordinate {
                    latitude: 25.005,
                    longitude: 121.505,
                },
            ),
            BikeShareStation::new(
                BikeShareStationId::new("station-dropoff"),
                "Dropoff Station",
                RequestCoordinate {
                    latitude: 25.010,
                    longitude: 121.510,
                },
            ),
        ],
    );
    let app = build_router_with_services_and_network(
        Arc::new(provider),
        Arc::new(InMemorySnapshotStore::new(snapshot)),
        None,
        None,
        network,
    )
    .await
    .unwrap();
    let mut bike_request = request();
    bike_request["selected_modes"] = json!(["bike_share"]);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(bike_request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();

    let option = &body["journey_options"][0];
    assert_eq!(option["class"], "feasible");
    assert_eq!(option["travel_modes"], json!(["walking", "bike_share"]));
    assert_eq!(option["legs"][1]["mode"], "bike_share");
    assert_eq!(option["legs"][1]["details"]["type"], "bike_share");
    assert_eq!(option["legs"][1]["details"]["available_bikes"], 2);
    assert_eq!(option["legs"][1]["details"]["available_return_slots"], 2);
}

#[tokio::test]
async fn planner_wide_provider_invariant_failure_returns_no_partial_result() {
    let provider = FakeProvider::builder(GraphRevision::new("revision-a"))
        .path_failure(ProviderError::MalformedResponse)
        .build();
    let app = build_router_with_services_and_network(
        Arc::new(provider),
        Arc::new(InMemorySnapshotStore::new(NetworkSnapshot {
            id: "snapshot-failure".into(),
            created_at: datetime!(2026-08-31 10:00 +08:00),
            live_cutoff: datetime!(2026-08-31 10:00 +08:00),
            static_network_revision: "static-1".into(),
            schedule_revision: "schedule-1".into(),
            freshness_policy_version: "1".into(),
            publication_version: 1,
            feed_evidence: Vec::new(),
            live_status: Vec::new(),
        })),
        None,
        None,
        TransitNetwork::default(),
    )
    .await
    .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(request().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

fn bike_status(
    id: &str,
    station_id: &str,
    available_bikes: u32,
    available_return_slots: u32,
    now: time::OffsetDateTime,
) -> NormalizedLiveStatus {
    NormalizedLiveStatus {
        id: id.into(),
        capability: "bike_availability_taipei".into(),
        received_at: now,
        normalized_at: now,
        freshness: Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: now,
        },
        value: LiveStatusValue::BikeAvailability(BikeAvailability {
            station_id: BikeShareStationId::new(station_id),
            rental_state: ActionState::Enabled,
            return_state: ActionState::Enabled,
            available_bikes: Some(available_bikes),
            available_return_slots: Some(available_return_slots),
            source_change_at: Some(now),
            provider_published_at: Some(now),
            received_at: now,
        }),
    }
}
