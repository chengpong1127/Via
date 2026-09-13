use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::macros::datetime;
use tower::ServiceExt;
use via_server::{
    api::RequestCoordinate,
    app::{build_router_with_network, build_router_with_realtime_and_network},
    live_status::{Freshness, FreshnessReason, FreshnessState},
    normalization::{
        ActionState, ArrivalEstimate, ArrivalState, BikeAvailability, BikeShareStationId,
        BoardingPointId, ScheduledRunId, TransitServiceId,
    },
    realtime::{
        ArrivalQuery, NormalizedRealtimeValue, RealtimeError, RealtimeProvider, RealtimeValue,
    },
    street::{
        Coordinate, DetailedPath, FakeProvider, GraphRevision, Maneuver, Metres, ProviderError,
        Seconds, StreetTravelMode,
    },
    transit::{
        BikeShareStation, ScheduledStopTime, ScheduledTrip, TransferEdge, TransitMode,
        TransitNetwork, TransitService, TransitStop,
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
        "origin": {"coordinate": {"latitude": 25.000, "longitude": 121.500}, "display_text": "Origin"},
        "destination": {"coordinate": {"latitude": 25.020, "longitude": 121.520}, "display_text": "Destination"},
        "selected_modes": ["transit"],
        "locale": "en",
        "departure_setting": {"type": "depart_at", "time": "2026-08-31T10:00:00+08:00"}
    })
}

fn network(mode: TransitMode, service_id: &str, first_id: &str, second_id: &str) -> TransitNetwork {
    let first = BoardingPointId::new(first_id);
    let second = BoardingPointId::new(second_id);
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![
            TransitStop::new(
                first.clone(),
                "First Stop",
                RequestCoordinate {
                    latitude: 25.010,
                    longitude: 121.510,
                },
            ),
            TransitStop::new(
                second.clone(),
                "Second Stop",
                RequestCoordinate {
                    latitude: 25.015,
                    longitude: 121.515,
                },
            ),
        ],
        vec![TransitService::new(
            TransitServiceId::new(service_id),
            mode,
            "Service",
            "Second Stop",
            vec![first, second],
        )],
        vec![ScheduledTrip::weekdays(
            ScheduledRunId::new("via_run_1"),
            TransitServiceId::new(service_id),
            vec![time::Weekday::Monday],
            vec![
                ScheduledStopTime::new(10 * 3600 + 600, 10 * 3600 + 600),
                ScheduledStopTime::new(10 * 3600 + 1_200, 10 * 3600 + 1_200),
            ],
        )],
        vec![BikeShareStation::new(
            BikeShareStationId::new("unused-bike-station"),
            "Unused Bike Station",
            RequestCoordinate {
                latitude: 25.010,
                longitude: 121.510,
            },
        )],
    )
    .with_revisions("static-1", "schedule-1")
}

#[derive(Clone, Default)]
struct FixtureRealtime {
    values: Vec<NormalizedRealtimeValue>,
    arrival_queries: Arc<Mutex<Vec<ArrivalQuery>>>,
    bike_queries: Arc<Mutex<Vec<Vec<BikeShareStationId>>>>,
    service_condition_values: Vec<NormalizedRealtimeValue>,
    service_condition_timeout: bool,
}

#[async_trait]
impl RealtimeProvider for FixtureRealtime {
    async fn arrival_estimates(
        &self,
        query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.arrival_queries.lock().unwrap().push(query);
        Ok(self.values.clone())
    }

    async fn bike_availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        self.bike_queries.lock().unwrap().push(station_ids.to_vec());
        Ok(self.values.clone())
    }

    async fn service_conditions(
        &self,
        _service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        if self.service_condition_timeout {
            return Err(RealtimeError::TimedOut);
        }
        Ok(self.service_condition_values.clone())
    }
}

fn normalized(value: RealtimeValue, now: time::OffsetDateTime) -> NormalizedRealtimeValue {
    NormalizedRealtimeValue {
        id: "fixture-live-status".into(),
        received_at: now,
        normalized_at: now,
        provider_published_at: Some(now),
        source_change_at: None,
        freshness: Freshness {
            state: FreshnessState::Fresh,
            reason: None,
            policy_version: "1".into(),
            evaluated_at: now,
        },
        value,
    }
}

async fn response_body(app: axum::Router, body: Value) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn transit_candidate_uses_static_schedule_without_a_global_snapshot() {
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
    let realtime = FixtureRealtime::default();
    let bike_queries = realtime.bike_queries.clone();
    let app = build_router_with_realtime_and_network(
        Arc::new(provider),
        Arc::new(realtime),
        network(
            TransitMode::Metro,
            "via_metro_blue",
            "via_boarding_1",
            "via_boarding_2",
        ),
    )
    .await
    .unwrap();
    let body = response_body(app, request()).await;
    assert_eq!(body["journey_options"][0]["class"], "feasible");
    assert_eq!(
        body["network_revision"]["static_network_revision"],
        "static-1"
    );
    assert_eq!(body["network_revision"]["schedule_revision"], "schedule-1");
    assert!(bike_queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn metro_candidate_degrades_to_schedule_only_when_realtime_times_out() {
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
    let app = build_router_with_realtime_and_network(
        Arc::new(provider),
        Arc::new(FixtureRealtime {
            service_condition_timeout: true,
            ..Default::default()
        }),
        network(
            TransitMode::Metro,
            "via_metro_blue",
            "via_boarding_1",
            "via_boarding_2",
        ),
    )
    .await
    .unwrap();
    let body = response_body(app, request()).await;
    assert_eq!(body["journey_options"][0]["class"], "feasible");
    assert_eq!(
        body["journey_options"][0]["live_status_summary"]["state"],
        "not_applicable"
    );
}

#[tokio::test]
async fn stale_optional_service_condition_does_not_remove_schedule_route() {
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
    let mut stale = normalized(
        RealtimeValue::ServiceCondition(via_server::normalization::ServiceCondition {
            state: via_server::normalization::ServiceConditionState::Disrupted,
            scope_via_ids: vec!["via_metro_blue".into()],
            source_text_variants: Vec::new(),
            effective_from: None,
            effective_until: None,
        }),
        now,
    );
    stale.freshness.state = FreshnessState::Degraded;
    stale.freshness.reason = Some(FreshnessReason::Stale);
    let app = build_router_with_realtime_and_network(
        Arc::new(provider),
        Arc::new(FixtureRealtime {
            service_condition_values: vec![stale],
            ..Default::default()
        }),
        network(
            TransitMode::Metro,
            "via_metro_blue",
            "via_boarding_1",
            "via_boarding_2",
        ),
    )
    .await
    .unwrap();
    let body = response_body(app, request()).await;
    assert_eq!(body["journey_options"][0]["class"], "feasible");
}

#[tokio::test]
async fn bus_candidate_is_enriched_only_for_the_scheduled_ride() {
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
    let realtime = FixtureRealtime {
        values: vec![normalized(
            RealtimeValue::ArrivalEstimate(ArrivalEstimate {
                transit_service_id: TransitServiceId::new("via_bus_1"),
                boarding_point_id: BoardingPointId::new("via_bus_boarding_1"),
                predicted_arrival_at: Some(datetime!(2026-08-31 10:12 +08:00)),
                state: ArrivalState::Predicted,
                transmitted_at: now,
            }),
            now,
        )],
        ..Default::default()
    };
    let arrival_queries = realtime.arrival_queries.clone();
    let app = build_router_with_realtime_and_network(
        Arc::new(provider),
        Arc::new(realtime),
        network(
            TransitMode::Bus,
            "via_bus_1",
            "via_bus_boarding_1",
            "via_bus_boarding_2",
        ),
    )
    .await
    .unwrap();
    let body = response_body(app, request()).await;
    assert_eq!(
        body["journey_options"][0]["legs"][1]["details"]["transit_mode"],
        "bus"
    );
    assert_eq!(
        body["journey_options"][0]["legs"][1]["departure_time"],
        "2026-08-31T10:12:00+08:00"
    );
    let queries = arrival_queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0].transit_service_ids,
        vec![TransitServiceId::new("via_bus_1")]
    );
    assert_eq!(
        queries[0].boarding_point_ids,
        vec![BoardingPointId::new("via_bus_boarding_1")]
    );
}

#[tokio::test]
async fn youbike_candidate_uses_demand_fetched_station_counts() {
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
    let value = |id: &str, bikes: u32, slots: u32| {
        normalized(
            RealtimeValue::BikeAvailability(BikeAvailability {
                station_id: BikeShareStationId::new(id),
                rental_state: ActionState::Enabled,
                return_state: ActionState::Enabled,
                available_bikes: Some(bikes),
                available_return_slots: Some(slots),
                source_change_at: Some(now),
                provider_published_at: Some(now),
                received_at: now,
            }),
            now,
        )
    };
    let realtime = FixtureRealtime {
        values: vec![
            value("station-pickup", 2, 4),
            value("station-dropoff", 4, 2),
        ],
        ..Default::default()
    };
    let bike_queries = realtime.bike_queries.clone();
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![
            BikeShareStation::new(
                BikeShareStationId::new("station-pickup"),
                "Pickup",
                RequestCoordinate {
                    latitude: 25.005,
                    longitude: 121.505,
                },
            ),
            BikeShareStation::new(
                BikeShareStationId::new("station-dropoff"),
                "Dropoff",
                RequestCoordinate {
                    latitude: 25.010,
                    longitude: 121.510,
                },
            ),
        ],
    )
    .with_revisions("static-1", "schedule-1");
    let app =
        build_router_with_realtime_and_network(Arc::new(provider), Arc::new(realtime), network)
            .await
            .unwrap();
    let mut body = request();
    body["selected_modes"] = json!(["bike_share"]);
    let result = response_body(app, body).await;
    assert_eq!(
        result["journey_options"][0]["legs"][1]["details"]["available_bikes"],
        2
    );
    assert_eq!(
        result["journey_options"][0]["legs"][1]["details"]["available_return_slots"],
        2
    );
    let queried_stations = bike_queries.lock().unwrap();
    assert_eq!(queried_stations.len(), 2);
    assert!(
        queried_stations
            .iter()
            .all(|station_ids| station_ids.len() == 1)
    );
    let queried_station_ids = queried_stations
        .iter()
        .flatten()
        .map(|station_id| station_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        queried_station_ids,
        std::collections::HashSet::from(["station-pickup", "station-dropoff"])
    );
}

#[tokio::test]
async fn planner_provider_failure_does_not_create_a_partial_internal_result() {
    let provider = FakeProvider::builder(GraphRevision::new("revision-a"))
        .path_failure(ProviderError::MalformedResponse)
        .build();
    let app = build_router_with_network(
        Arc::new(provider),
        TransitNetwork::default().with_revisions("static-1", "schedule-1"),
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
    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn unified_search_can_use_bike_share_before_and_after_transit() {
    let revision = GraphRevision::new("revision-a");
    let origin = coordinate(25.000, 121.500);
    let destination = coordinate(25.040, 121.540);
    let a = BoardingPointId::new("a");
    let b = BoardingPointId::new("b");
    let c = BoardingPointId::new("c");
    let d = BoardingPointId::new("d");
    let transit_return = coordinate(25.011, 121.511);
    let transit_pickup = coordinate(25.031, 121.531);
    let origin_pickup = coordinate(25.003, 121.503);
    let destination_return = coordinate(25.037, 121.537);
    let network = TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![
            TransitStop::new(
                a.clone(),
                "A",
                RequestCoordinate {
                    latitude: 25.010,
                    longitude: 121.510,
                },
            ),
            TransitStop::new(
                b.clone(),
                "B",
                RequestCoordinate {
                    latitude: 25.020,
                    longitude: 121.520,
                },
            ),
            TransitStop::new(
                c.clone(),
                "C",
                RequestCoordinate {
                    latitude: 25.030,
                    longitude: 121.530,
                },
            ),
            TransitStop::new(
                d.clone(),
                "D",
                RequestCoordinate {
                    latitude: 25.035,
                    longitude: 121.535,
                },
            ),
        ],
        vec![
            TransitService::new(
                TransitServiceId::new("bus"),
                TransitMode::Bus,
                "307",
                "B",
                vec![a.clone(), b.clone()],
            ),
            TransitService::new(
                TransitServiceId::new("metro"),
                TransitMode::Metro,
                "Blue",
                "D",
                vec![c.clone(), d.clone()],
            ),
        ],
        vec![
            ScheduledTrip::weekdays(
                ScheduledRunId::new("bus-run"),
                TransitServiceId::new("bus"),
                vec![time::Weekday::Monday],
                vec![
                    ScheduledStopTime::new(36_600, 36_600),
                    ScheduledStopTime::new(37_200, 37_200),
                ],
            ),
            ScheduledTrip::weekdays(
                ScheduledRunId::new("metro-run"),
                TransitServiceId::new("metro"),
                vec![time::Weekday::Monday],
                vec![
                    ScheduledStopTime::new(37_500, 37_500),
                    ScheduledStopTime::new(38_100, 38_100),
                ],
            ),
        ],
        vec![
            BikeShareStation::new(
                BikeShareStationId::new("origin-pickup"),
                "Origin Pickup",
                RequestCoordinate {
                    latitude: origin_pickup.latitude(),
                    longitude: origin_pickup.longitude(),
                },
            ),
            BikeShareStation::new(
                BikeShareStationId::new("transit-return"),
                "Transit Return",
                RequestCoordinate {
                    latitude: transit_return.latitude(),
                    longitude: transit_return.longitude(),
                },
            ),
            BikeShareStation::new(
                BikeShareStationId::new("transit-pickup"),
                "Transit Pickup",
                RequestCoordinate {
                    latitude: transit_pickup.latitude(),
                    longitude: transit_pickup.longitude(),
                },
            ),
            BikeShareStation::new(
                BikeShareStationId::new("destination-return"),
                "Destination Return",
                RequestCoordinate {
                    latitude: destination_return.latitude(),
                    longitude: destination_return.longitude(),
                },
            ),
        ],
    )
    .with_transfer_edges(vec![TransferEdge::new(b.clone(), c.clone(), 60)])
    .with_revisions("static-1", "schedule-1");
    let path_for =
        |from: Coordinate, to: Coordinate, seconds| path(revision.clone(), from, to, seconds);
    let provider = FakeProvider::builder(revision.clone())
        .path(
            StreetTravelMode::Walking,
            origin,
            coordinate(25.010, 121.510),
            path_for(origin, coordinate(25.010, 121.510), 600),
        )
        .path(
            StreetTravelMode::Walking,
            coordinate(25.035, 121.535),
            destination,
            path_for(coordinate(25.035, 121.535), destination, 600),
        )
        .path(
            StreetTravelMode::Walking,
            origin,
            origin_pickup,
            path_for(origin, origin_pickup, 60),
        )
        .path(
            StreetTravelMode::Bicycle,
            origin_pickup,
            transit_return,
            path_for(origin_pickup, transit_return, 300),
        )
        .path(
            StreetTravelMode::Walking,
            transit_return,
            coordinate(25.010, 121.510),
            path_for(transit_return, coordinate(25.010, 121.510), 60),
        )
        .path(
            StreetTravelMode::Walking,
            coordinate(25.035, 121.535),
            transit_pickup,
            path_for(coordinate(25.035, 121.535), transit_pickup, 60),
        )
        .path(
            StreetTravelMode::Bicycle,
            transit_pickup,
            destination_return,
            path_for(transit_pickup, destination_return, 300),
        )
        .path(
            StreetTravelMode::Walking,
            destination_return,
            destination,
            path_for(destination_return, destination, 60),
        )
        .build();
    let now = datetime!(2026-08-31 10:00 +08:00);
    let bike_value = |id: &str| {
        normalized(
            RealtimeValue::BikeAvailability(BikeAvailability {
                station_id: BikeShareStationId::new(id),
                rental_state: ActionState::Enabled,
                return_state: ActionState::Enabled,
                available_bikes: Some(4),
                available_return_slots: Some(4),
                source_change_at: Some(now),
                provider_published_at: Some(now),
                received_at: now,
            }),
            now,
        )
    };
    let realtime = FixtureRealtime {
        values: vec![
            normalized(
                RealtimeValue::ArrivalEstimate(ArrivalEstimate {
                    transit_service_id: TransitServiceId::new("bus"),
                    boarding_point_id: a,
                    predicted_arrival_at: Some(datetime!(2026-08-31 10:10 +08:00)),
                    state: ArrivalState::Predicted,
                    transmitted_at: now,
                }),
                now,
            ),
            bike_value("origin-pickup"),
            bike_value("transit-return"),
            bike_value("transit-pickup"),
            bike_value("destination-return"),
        ],
        ..Default::default()
    };
    let app =
        build_router_with_realtime_and_network(Arc::new(provider), Arc::new(realtime), network)
            .await
            .unwrap();
    let result = response_body(
        app,
        json!({
            "origin": {"coordinate": {"latitude": 25.000, "longitude": 121.500}, "display_text": "Origin"},
            "destination": {"coordinate": {"latitude": 25.040, "longitude": 121.540}, "display_text": "Destination"},
            "selected_modes": ["bike_share", "transit"],
            "locale": "en",
            "departure_setting": {"type": "depart_at", "time": "2026-08-31T10:00:00+08:00"}
        }),
    )
    .await;
    let options = result["journey_options"].as_array().unwrap();
    let mode_sequences = options
        .iter()
        .map(|option| {
            option["legs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|leg| leg["mode"].as_str())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(mode_sequences.iter().any(|modes| {
        modes
            .iter()
            .position(|mode| *mode == "transit")
            .zip(modes.iter().position(|mode| *mode == "bike_share"))
            .is_some_and(|(transit, bike)| transit < bike)
    }));
}

#[tokio::test]
async fn bus_topology_can_create_a_departure_without_a_static_timetable_trip() {
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
    let realtime = FixtureRealtime {
        values: vec![normalized(
            RealtimeValue::ArrivalEstimate(ArrivalEstimate {
                transit_service_id: TransitServiceId::new("via_bus_1"),
                boarding_point_id: BoardingPointId::new("via_bus_boarding_1"),
                predicted_arrival_at: Some(datetime!(2026-08-31 10:12 +08:00)),
                state: ArrivalState::Predicted,
                transmitted_at: now,
            }),
            now,
        )],
        ..Default::default()
    };
    let mut network = network(
        TransitMode::Bus,
        "via_bus_1",
        "via_bus_boarding_1",
        "via_bus_boarding_2",
    );
    network.trips.clear();
    let app =
        build_router_with_realtime_and_network(Arc::new(provider), Arc::new(realtime), network)
            .await
            .unwrap();
    let body = response_body(app, request()).await;
    assert_eq!(
        body["journey_options"][0]["legs"][1]["details"]["transit_mode"],
        "bus"
    );
    assert_eq!(
        body["journey_options"][0]["legs"][1]["departure_time"],
        "2026-08-31T10:12:00+08:00"
    );
}
