use std::sync::Arc;

use crate::support::{build_test_router_with_metrics, build_test_router_with_network_and_metrics};
use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL, header::CONTENT_TYPE},
};
use http_body_util::BodyExt;
use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::{Value, json};
use tower::ServiceExt;
use via_server::domain::transit::TransitNetwork;
use via_server::street::FakeStreetRouter;

#[tokio::test]
async fn metrics_endpoint_renders_the_prometheus_recorder() {
    let provider = Arc::new(FakeStreetRouter::builder().build());
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    ::metrics::with_local_recorder(&recorder, || {
        ::metrics::counter!("via_metrics_endpoint_test_total").increment(1);
    });
    let app = build_test_router_with_metrics(provider, handle.clone())
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let content_type = response.headers()[CONTENT_TYPE].to_str().unwrap();
    assert!(content_type.starts_with("text/plain; version=0.0.4"));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let rendered = handle.render();
    assert_eq!(body.as_ref(), rendered.as_bytes());
    assert!(rendered.contains("via_metrics_endpoint_test_total 1"));
}

fn planning_request() -> Value {
    json!({
        "origin": { "coordinate": { "latitude": 25.04776, "longitude": 121.51706 }, "display_text": "Origin" },
        "destination": { "coordinate": { "latitude": 25.03396, "longitude": 121.56447 }, "display_text": "Destination" },
        "selected_modes": [],
        "departure_setting": { "type": "depart_now" }
    })
}

async fn planning_response(provider: FakeStreetRouter, body: Value) -> (StatusCode, String) {
    planning_response_with_network(
        provider,
        body,
        TransitNetwork::default().with_revisions("development-static", "development-schedule"),
    )
    .await
}

async fn planning_response_with_network(
    provider: FakeStreetRouter,
    body: Value,
    network: TransitNetwork,
) -> (StatusCode, String) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let app =
        build_test_router_with_network_and_metrics(Arc::new(provider), network, recorder.handle())
            .await
            .unwrap();
    let _guard = ::metrics::set_default_local_recorder(&recorder);
    let status = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/journey-plans")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    (status, recorder.handle().render())
}

fn metric_line<'a>(rendered: &'a str, name: &str) -> Option<&'a str> {
    rendered.lines().find(|line| line.starts_with(name))
}

fn assert_counter(rendered: &str, name: &str, value: u64) {
    assert_eq!(
        metric_line(rendered, name),
        Some(format!("{name} {value}").as_str())
    );
}

fn assert_metric_positive(rendered: &str, name: &str) {
    let value = metric_line(rendered, name)
        .and_then(|line| line.split_whitespace().last())
        .and_then(|value| value.parse::<f64>().ok())
        .expect("metric should be rendered with a numeric value");
    assert!(value > 0.0, "expected {name} to be positive, got {value}");
}

#[tokio::test(flavor = "current_thread")]
async fn planning_request_counter_counts_successful_options_once() {
    let origin = via_server::street::Coordinate::try_new(25.04776, 121.51706).unwrap();
    let destination = via_server::street::Coordinate::try_new(25.03396, 121.56447).unwrap();
    let provider = FakeStreetRouter::builder()
        .path(
            via_server::street::StreetTravelMode::Walking,
            origin,
            destination,
            via_server::street::StreetPath {
                duration: via_server::street::Seconds(620),
                distance: via_server::street::Metres(3900),
                geometry: vec![origin, destination],
                maneuvers: vec![
                    via_server::street::Maneuver::depart(0),
                    via_server::street::Maneuver::arrive(1),
                ],
                attribution: vec![],
            },
        )
        .build();
    let (status, rendered) = planning_response(provider, planning_request()).await;
    assert_eq!(status, StatusCode::OK);
    assert_counter(&rendered, "via_planning_requests_total", 1);
    assert_counter(&rendered, "via_planning_options_total", 1);
    assert_counter(
        &rendered,
        "via_planner_multimodal_search_duration_seconds_count",
        1,
    );
    assert!(
        metric_line(
            &rendered,
            "via_planner_candidate_discovery_duration_seconds_count"
        )
        .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn planning_request_counter_counts_empty_and_failures_once_and_ignores_invalid_requests() {
    let (empty_status, empty_metrics) =
        planning_response(FakeStreetRouter::builder().build(), planning_request()).await;
    assert_eq!(empty_status, StatusCode::OK);
    assert_counter(&empty_metrics, "via_planning_requests_total", 1);
    assert_counter(&empty_metrics, "via_planning_empty_total", 1);
    assert_counter(
        &empty_metrics,
        "via_planner_multimodal_search_duration_seconds_count",
        1,
    );
    assert!(
        metric_line(
            &empty_metrics,
            "via_planner_candidate_discovery_duration_seconds_count"
        )
        .is_none()
    );

    let mut invalid = planning_request();
    invalid["origin"]["coordinate"]["latitude"] = json!(91);
    let (invalid_status, invalid_metrics) =
        planning_response(FakeStreetRouter::builder().build(), invalid).await;
    assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
    assert!(metric_line(&invalid_metrics, "via_planning_requests_total").is_none());

    let failed = FakeStreetRouter::builder()
        .route_failure(via_server::street::StreetRoutingError::Timeout)
        .build();
    let (failure_status, failure_metrics) = planning_response(failed, planning_request()).await;
    assert_eq!(failure_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_counter(&failure_metrics, "via_planning_requests_total", 1);
    assert_counter(&failure_metrics, "via_planning_failures_total", 1);
}

#[tokio::test(flavor = "current_thread")]
async fn discovery_failure_is_counted_as_one_completed_planning_request() {
    let mut request = planning_request();
    request["origin"]["coordinate"] = json!({"latitude": 25.11, "longitude": 121.51});
    request["destination"]["coordinate"] = json!({"latitude": 25.111, "longitude": 121.511});
    request["selected_modes"] = json!(["bus"]);
    let provider = FakeStreetRouter::builder()
        .matrix_failure(via_server::street::StreetRoutingError::Timeout)
        .build();
    let network =
        TransitNetwork::from_artifact_json(include_str!("../../fixtures/routing/network.json"))
            .unwrap();
    let (status, rendered) = planning_response_with_network(provider, request, network).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_counter(&rendered, "via_planning_requests_total", 1);
    assert_counter(&rendered, "via_planning_failures_total", 1);
    assert_counter(&rendered, "via_planner_transit_candidate_stops_total", 0);
    assert_counter(
        &rendered,
        "via_planner_candidate_discovery_duration_seconds_count",
        1,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn successful_transit_discovery_records_candidate_stops() {
    let network =
        TransitNetwork::from_artifact_json(include_str!("../../fixtures/routing/network.json"))
            .unwrap();
    let origin = via_server::street::Coordinate::try_new(25.1101, 121.5101).unwrap();
    let bus_a = via_server::street::Coordinate::try_new(25.11, 121.51).unwrap();
    let bus_b = via_server::street::Coordinate::try_new(25.111, 121.511).unwrap();
    let destination = via_server::street::Coordinate::try_new(25.1111, 121.5111).unwrap();
    let provider = FakeStreetRouter::builder()
        .matrix_leg(
            via_server::street::StreetTravelMode::Walking,
            origin,
            bus_a,
            via_server::street::Seconds(30),
            via_server::street::Metres(15),
        )
        .matrix_leg(
            via_server::street::StreetTravelMode::Walking,
            bus_b,
            destination,
            via_server::street::Seconds(30),
            via_server::street::Metres(15),
        )
        .build();
    let mut request = planning_request();
    request["origin"]["coordinate"] =
        json!({"latitude": origin.latitude(), "longitude": origin.longitude()});
    request["destination"]["coordinate"] =
        json!({"latitude": destination.latitude(), "longitude": destination.longitude()});
    request["selected_modes"] = json!(["bus"]);
    let (_, rendered) = planning_response_with_network(provider, request, network).await;
    assert_counter(&rendered, "via_planning_requests_total", 1);
    assert_counter(
        &rendered,
        "via_planner_candidate_discovery_duration_seconds_count",
        1,
    );
    assert_metric_positive(&rendered, "via_planner_transit_candidate_stops_total");
}
