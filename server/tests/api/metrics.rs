use std::sync::Arc;

use crate::support::build_test_router_with_metrics;
use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL, header::CONTENT_TYPE},
};
use http_body_util::BodyExt;
use metrics_exporter_prometheus::PrometheusBuilder;
use tower::ServiceExt;
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
