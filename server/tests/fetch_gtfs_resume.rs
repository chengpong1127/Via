use std::{
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, Response, StatusCode},
    routing::any,
};
use bytes::Bytes;
use via_server::{
    config::ViaConfig,
    data::fetch::{FetchError, FetchPolicy, FetchSources, fetch_snapshot_at},
    tdx::{TdxClient, TdxCredentials, TdxSettings},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Copy, PartialEq)]
enum GtfsBehaviour {
    CutThenResume,
    CutThenFileChanged,
    ResumeWithWrongContentRange,
    ResumeWithRangeNotSatisfiable,
    AlwaysUnavailable,
    AlwaysNotFound,
    RateLimitedOnce,
}

#[derive(Clone)]
struct Mock {
    behaviour: GtfsBehaviour,
    original: Vec<u8>,
    replacement: Vec<u8>,
    gtfs_requests: Arc<Mutex<Vec<GtfsRequest>>>,
}

#[derive(Clone, Debug)]
struct GtfsRequest {
    at: Instant,
    range: Option<String>,
    if_range: Option<String>,
}

fn zip_with(marker: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "stops.txt",
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(format!("stop_id,stop_name\n{marker}\n").as_bytes())
        .unwrap();
    zip.write_all(&vec![b'x'; 200_000]).unwrap();
    zip.finish().unwrap().into_inner()
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn full(body: &[u8], etag: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("etag", etag)
        .header("content-length", body.len())
        .body(Body::from(body.to_vec()))
        .unwrap()
}

fn cut_after_half(body: &[u8], etag: &str) -> Response<Body> {
    let half = body.len() / 2;
    let first = Bytes::copy_from_slice(&body[..half]);
    let stream = futures_util::StreamExt::chain(
        futures_util::stream::iter(vec![Ok::<Bytes, std::io::Error>(first)]),
        futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Err(std::io::Error::other("connection cut"))
        }),
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("etag", etag)
        .header("content-length", body.len())
        .body(Body::from_stream(stream))
        .unwrap()
}

fn partial(body: &[u8], start: usize, etag: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header("etag", etag)
        .header(
            "content-range",
            format!("bytes {start}-{}/{}", body.len() - 1, body.len()),
        )
        .header("content-length", body.len() - start)
        .body(Body::from(body[start..].to_vec()))
        .unwrap()
}

fn range_start(range: &str) -> usize {
    range
        .strip_prefix("bytes=")
        .and_then(|rest| rest.strip_suffix('-'))
        .unwrap()
        .parse()
        .unwrap()
}

async fn respond(State(mock): State<Mock>, request: Request<Body>) -> Response<Body> {
    let path = request.uri().path().to_owned();
    if path.starts_with("/token") {
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(r#"{"access_token":"t","expires_in":300}"#))
            .unwrap();
    }
    if path.contains("/Bike/Station/City/") {
        let first_page = request
            .uri()
            .query()
            .is_some_and(|query| query.contains("%24skip=0") || query.contains("$skip=0"));
        let city = path.rsplit('/').next().unwrap();
        let body = if first_page {
            format!(
                r#"[{{"StationUID":"U-{city}","StationName":{{"Zh_tw":"{city}"}},"StationPosition":{{"PositionLat":25.0,"PositionLon":121.0}}}}]"#
            )
        } else {
            "[]".to_owned()
        };
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
    }
    if path.contains("/Bus/Route/City/") || path.contains("/Bus/Schedule/City/") {
        return Response::builder().body(Body::from("[]")).unwrap();
    }
    if path != "/gtfs" {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap();
    }
    let range = header(request.headers(), "range");
    let if_range = header(request.headers(), "if-range");
    let attempt = {
        let mut requests = mock.gtfs_requests.lock().unwrap();
        requests.push(GtfsRequest {
            at: Instant::now(),
            range: range.clone(),
            if_range: if_range.clone(),
        });
        requests.len()
    };
    let (original, replacement) = (&mock.original, &mock.replacement);
    match (mock.behaviour, attempt) {
        (GtfsBehaviour::CutThenResume, 1) => cut_after_half(original, "\"v1\""),
        (GtfsBehaviour::CutThenResume, _) => match (range, if_range.as_deref()) {
            (Some(range), Some("\"v1\"")) => partial(original, range_start(&range), "\"v1\""),
            _ => full(original, "\"v1\""),
        },
        (GtfsBehaviour::CutThenFileChanged, 1) => cut_after_half(original, "\"v1\""),
        (GtfsBehaviour::CutThenFileChanged, _) => full(replacement, "\"v2\""),
        (GtfsBehaviour::ResumeWithWrongContentRange, 1) => cut_after_half(original, "\"v1\""),
        (GtfsBehaviour::ResumeWithWrongContentRange, 2) => partial(original, 0, "\"v1\""),
        (GtfsBehaviour::ResumeWithWrongContentRange, _) => full(original, "\"v1\""),
        (GtfsBehaviour::ResumeWithRangeNotSatisfiable, 1) => cut_after_half(original, "\"v1\""),
        (GtfsBehaviour::ResumeWithRangeNotSatisfiable, 2) => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .body(Body::empty())
            .unwrap(),
        (GtfsBehaviour::ResumeWithRangeNotSatisfiable, _) => full(original, "\"v1\""),
        (GtfsBehaviour::AlwaysUnavailable, _) => Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Body::empty())
            .unwrap(),
        (GtfsBehaviour::AlwaysNotFound, _) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap(),
        (GtfsBehaviour::RateLimitedOnce, 1) => Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("retry-after", "1")
            .body(Body::empty())
            .unwrap(),
        (GtfsBehaviour::RateLimitedOnce, _) => full(original, "\"v1\""),
    }
}

struct Harness {
    client: TdxClient,
    sources: FetchSources,
    mock: Mock,
    root: PathBuf,
}

async fn harness(behaviour: GtfsBehaviour, interval: Duration) -> Harness {
    let mock = Mock {
        behaviour,
        original: zip_with("original"),
        replacement: zip_with("replacement"),
        gtfs_requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .fallback(any(respond))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = TdxClient::new(
        TdxCredentials::from_client_credentials("id", "secret"),
        TdxSettings {
            api_base_url: format!("{base}/basic/"),
            token_url: format!("{base}/token"),
            ..ViaConfig::default().tdx
        },
    )
    .unwrap();
    let sources = FetchSources {
        gtfs_static_url: format!("{base}/gtfs"),
        bike_api_base_url: format!("{base}/basic/"),
        policy: FetchPolicy {
            request_interval: interval,
            max_attempts: 3,
            retry_backoff: Duration::ZERO,
            gtfs_only: false,
        },
    };
    Harness {
        client,
        sources,
        mock,
        root: std::env::temp_dir().join(format!("via-fetch-int-{}", uuid::Uuid::new_v4().simple())),
    }
}

fn requests(harness: &Harness) -> Vec<GtfsRequest> {
    harness.mock.gtfs_requests.lock().unwrap().clone()
}

fn point_latest_at(root: &Path, name: &str) {
    let link = root.join("latest");
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(name, link).unwrap();
}

fn leftovers(root: &Path) -> Vec<String> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect()
}

#[tokio::test]
async fn a_mid_stream_cut_resumes_with_range_and_if_range() {
    let h = harness(GtfsBehaviour::CutThenResume, Duration::ZERO).await;
    let snapshot = fetch_snapshot_at(&h.root, "20260101T000000Z", None, &h.client, &h.sources)
        .await
        .unwrap();
    let seen = requests(&h);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].range, None);
    let resumed_from = range_start(seen[1].range.as_deref().expect("second request has Range"));
    assert!(resumed_from > 0 && resumed_from < h.mock.original.len());
    assert_eq!(seen[1].if_range.as_deref(), Some("\"v1\""));
    assert_eq!(
        fs::read(snapshot.path.join("gtfs.zip")).unwrap(),
        h.mock.original
    );
    fs::remove_dir_all(&h.root).unwrap();
}

#[tokio::test]
async fn a_changed_file_restarts_from_zero_instead_of_splicing() {
    let h = harness(GtfsBehaviour::CutThenFileChanged, Duration::ZERO).await;
    let snapshot = fetch_snapshot_at(&h.root, "20260101T000000Z", None, &h.client, &h.sources)
        .await
        .unwrap();
    let seen = requests(&h);
    assert_eq!(seen[1].if_range.as_deref(), Some("\"v1\""));
    assert_eq!(
        fs::read(snapshot.path.join("gtfs.zip")).unwrap(),
        h.mock.replacement
    );
    fs::remove_dir_all(&h.root).unwrap();
}

#[tokio::test]
async fn a_partial_response_that_starts_elsewhere_restarts_from_zero() {
    let h = harness(GtfsBehaviour::ResumeWithWrongContentRange, Duration::ZERO).await;
    let snapshot = fetch_snapshot_at(&h.root, "20260101T000000Z", None, &h.client, &h.sources)
        .await
        .unwrap();
    let seen = requests(&h);
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2].range, None);
    assert_eq!(
        fs::read(snapshot.path.join("gtfs.zip")).unwrap(),
        h.mock.original
    );
    fs::remove_dir_all(&h.root).unwrap();
}

#[tokio::test]
async fn a_range_not_satisfiable_answer_restarts_from_zero() {
    let h = harness(GtfsBehaviour::ResumeWithRangeNotSatisfiable, Duration::ZERO).await;
    let snapshot = fetch_snapshot_at(&h.root, "20260101T000000Z", None, &h.client, &h.sources)
        .await
        .unwrap();
    assert_eq!(
        fs::read(snapshot.path.join("gtfs.zip")).unwrap(),
        h.mock.original
    );
    fs::remove_dir_all(&h.root).unwrap();
}

#[tokio::test]
async fn gtfs_failure_after_all_attempts_leaves_latest_and_the_directory_untouched() {
    let h = harness(GtfsBehaviour::CutThenResume, Duration::ZERO).await;
    fetch_snapshot_at(&h.root, "20260101T000000Z", None, &h.client, &h.sources)
        .await
        .unwrap();
    point_latest_at(&h.root, "20260101T000000Z");
    let failing = harness(GtfsBehaviour::AlwaysUnavailable, Duration::ZERO).await;
    let error = fetch_snapshot_at(
        &h.root,
        "20260102T000000Z",
        None,
        &failing.client,
        &failing.sources,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, FetchError::Tdx(_)));
    assert_eq!(requests(&failing).len(), 3);
    assert!(!h.root.join("20260102T000000Z").exists());
    assert_eq!(
        fs::read_link(h.root.join("latest")).unwrap(),
        PathBuf::from("20260101T000000Z")
    );
    assert_eq!(leftovers(&h.root), Vec::<String>::new());
    fs::remove_dir_all(&h.root).unwrap();
}

#[tokio::test]
async fn a_permanent_gtfs_client_error_is_not_retried() {
    let h = harness(GtfsBehaviour::AlwaysNotFound, Duration::ZERO).await;
    let error = fetch_snapshot_at(&h.root, "20260101T000000Z", None, &h.client, &h.sources)
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Tdx(_)));
    assert_eq!(requests(&h).len(), 1);
    assert_eq!(leftovers(&h.root), Vec::<String>::new());
    fs::remove_dir_all(&h.root).unwrap();
}

#[tokio::test]
async fn retry_after_and_the_request_interval_both_bound_the_gtfs_retry_gap() {
    let h = harness(GtfsBehaviour::RateLimitedOnce, Duration::from_millis(20)).await;
    fetch_snapshot_at(&h.root, "20260101T000000Z", None, &h.client, &h.sources)
        .await
        .unwrap();
    let seen = requests(&h);
    assert_eq!(seen.len(), 2);
    assert!(seen[1].at - seen[0].at >= Duration::from_secs(1));
    fs::remove_dir_all(&h.root).unwrap();
}

#[test]
fn the_default_budget_spaces_calls_at_least_fifteen_seconds_apart() {
    let policy = FetchPolicy::from_settings(&ViaConfig::default().tdx.fetch, false);
    assert!(policy.request_interval >= Duration::from_secs(15));
}
