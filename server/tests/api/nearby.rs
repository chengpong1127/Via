use std::sync::Arc;

use crate::support::build_test_router_with_network;
use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL},
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use via_server::{
    domain::ids::{BoardingPointId, TransitPatternId, TransitRouteId},
    domain::{
        location::GeoCoordinate,
        transit::{
            SegmentProfile, SegmentTimeSource, SegmentTravelTime, TransitMode, TransitNetwork,
            TransitPattern, TransitRoute, TransitStop,
        },
    },
    street::FakeStreetRouter,
};

fn stop(id: &str, name: &str, latitude: f64) -> TransitStop {
    TransitStop::new(
        BoardingPointId::new(id),
        name,
        GeoCoordinate {
            latitude,
            longitude: 121.5,
        },
    )
}

fn pattern(id: &str, route: &str, stops: &[&str]) -> TransitPattern {
    TransitPattern::new(
        TransitPatternId::new(id),
        TransitRouteId::new(route),
        None,
        "Terminal",
        stops
            .iter()
            .map(|stop| BoardingPointId::new(*stop))
            .collect(),
        SegmentProfile {
            segments: vec![
                SegmentTravelTime {
                    seconds: 120,
                    source: SegmentTimeSource::Derived,
                };
                stops.len() - 1
            ],
        },
    )
}

fn network() -> TransitNetwork {
    TransitNetwork::new(
        time::Time::from_hms(4, 0, 0).unwrap(),
        vec![
            stop("near", "Near Stop", 25.0),
            stop("close", "Close Stop", 25.0027),
            stop("far", "Far Station", 25.05),
            stop("unserved", "Unserved", 25.0001),
            stop("near-platform", "Near Stop", 25.0002),
            stop("near-elsewhere", "Near Stop", 25.002),
        ],
        vec![
            pattern("bus-307", "bus:307", &["near", "close"]),
            pattern("metro-bl", "metro:BL", &["near", "far"]),
            pattern("bus-999", "bus:999", &["near-platform", "near-elsewhere"]),
        ],
        Vec::new(),
    )
    .with_routes(vec![
        TransitRoute {
            id: TransitRouteId::new("bus:307"),
            mode: TransitMode::Bus,
            label: "307".into(),
        },
        TransitRoute {
            id: TransitRouteId::new("bus:999"),
            mode: TransitMode::Bus,
            label: "999".into(),
        },
        TransitRoute {
            id: TransitRouteId::new("metro:BL"),
            mode: TransitMode::Metro,
            label: "BL".into(),
        },
    ])
    .with_revisions("static-1", "schedule-1")
}

async fn get(uri: &str) -> (StatusCode, Option<String>, Value) {
    let app =
        build_test_router_with_network(Arc::new(FakeStreetRouter::builder().build()), network())
            .await
            .unwrap();
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get(CACHE_CONTROL)
        .map(|value| value.to_str().unwrap().to_owned());
    let body =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, cache, body)
}

#[tokio::test]
async fn nearby_stops_lists_served_stops_within_the_radius_nearest_first() {
    let (status, cache, body) =
        get("/v1/nearby-stops?latitude=25.0&longitude=121.5&radius_metres=500").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert!(body["generated_at"].is_string());
    let stops = body["transit_stops"].as_array().unwrap();
    let ids: Vec<&Value> = stops.iter().map(|stop| &stop["via_ids"]).collect();
    assert_eq!(
        ids,
        [
            &serde_json::json!(["near", "near-platform"]),
            &serde_json::json!(["near-elsewhere"]),
            &serde_json::json!(["close"]),
        ]
    );
    assert_eq!(stops[0]["name"], "Near Stop");
    assert_eq!(stops[0]["distance_metres"], 0);
    assert_eq!(stops[0]["modes"], serde_json::json!(["bus", "metro"]));
    assert_eq!(
        stops[0]["route_labels"],
        serde_json::json!(["307", "999", "BL"])
    );
    assert_eq!(stops[1]["distance_metres"], 222);
    assert_eq!(stops[2]["distance_metres"], 300);
    assert_eq!(stops[2]["modes"], serde_json::json!(["bus"]));
    assert_eq!(body["bike_stations"], serde_json::json!([]));
}

#[tokio::test]
async fn nearby_stops_defaults_the_radius() {
    let (status, _, body) = get("/v1/nearby-stops?latitude=25.0&longitude=121.5").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["transit_stops"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn nearby_stops_rejects_invalid_queries() {
    for (uri, code, field) in [
        (
            "/v1/nearby-stops?latitude=95&longitude=121.5",
            "invalid_coordinate",
            "/latitude",
        ),
        (
            "/v1/nearby-stops?latitude=25&longitude=121.5&radius_metres=5000",
            "invalid_radius",
            "/radius_metres",
        ),
        (
            "/v1/nearby-stops?latitude=25&longitude=121.5&radius_metres=0",
            "invalid_radius",
            "/radius_metres",
        ),
        ("/v1/nearby-stops?latitude=25", "invalid_request", "/"),
    ] {
        let (status, cache, body) = get(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(cache.as_deref(), Some("no-store"));
        assert_eq!(body["error"]["code"], code, "{uri}");
        assert_eq!(body["error"]["field"], field, "{uri}");
    }
}
