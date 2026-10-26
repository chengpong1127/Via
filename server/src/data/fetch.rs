use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use zip::ZipArchive;

use crate::tdx::{BIKE_CITIES, TdxClient, TdxClientError};

pub const TDX_GTFS_STATIC_URL: &str = "https://tdx.transportdata.tw/api/gtfs/V3/Map/GTFS/Static";
const BIKE_STATION_ORDER: &str = "StationUID asc";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchSource {
    pub source_id: String,
    pub url: Option<String>,
    pub sha256: String,
    pub byte_size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchManifest {
    pub manifest_version: u32,
    pub fetched_at: String,
    pub sources: BTreeMap<String, FetchSource>,
}

#[derive(Clone, Debug)]
pub struct FetchSnapshot {
    pub path: PathBuf,
    pub manifest: FetchManifest,
    pub bike_station_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("fetch snapshot path {0} could not be read or written: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("TDX fetch failed: {0}")]
    Tdx(#[from] TdxClientError),
    #[error("TDX Bike Station response for {city} was invalid: {detail}")]
    InvalidBikeResponse { city: String, detail: String },
    #[error("downloaded GTFS data is not a valid ZIP archive")]
    InvalidGtfsArchive,
    #[error("fetch snapshot timestamp {0} is invalid")]
    InvalidTimestamp(String),
    #[error("fetch snapshot already exists at {0}")]
    SnapshotExists(PathBuf),
}

#[derive(Clone, Debug)]
pub struct FetchSources {
    pub gtfs_static_url: String,
    pub bike_api_base_url: String,
}

impl FetchSources {
    pub fn production(bike_api_base_url: impl Into<String>) -> Self {
        Self {
            gtfs_static_url: TDX_GTFS_STATIC_URL.to_owned(),
            bike_api_base_url: bike_api_base_url.into(),
        }
    }
}

pub async fn fetch_snapshot(
    gtfs_file: Option<&Path>,
    client: &TdxClient,
    sources: &FetchSources,
) -> Result<FetchSnapshot, FetchError> {
    let snapshot_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/fetch");
    fetch_snapshot_at(&snapshot_root, &timestamp_now(), gtfs_file, client, sources).await
}

pub async fn fetch_snapshot_at(
    root: &Path,
    timestamp: &str,
    gtfs_file: Option<&Path>,
    client: &TdxClient,
    sources: &FetchSources,
) -> Result<FetchSnapshot, FetchError> {
    if !valid_timestamp(timestamp) {
        return Err(FetchError::InvalidTimestamp(timestamp.to_owned()));
    }
    fs::create_dir_all(root).map_err(|error| FetchError::Io(root.into(), error))?;
    let destination = root.join(timestamp);
    if destination.exists() {
        return Err(FetchError::SnapshotExists(destination));
    }
    let temporary = root.join(format!(
        ".{timestamp}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&temporary).map_err(|error| FetchError::Io(temporary.clone(), error))?;
    let result = populate_snapshot(&temporary, timestamp, gtfs_file, client, sources).await;
    let snapshot = match result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error);
        }
    };
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(FetchError::Io(destination, error));
    }
    Ok(FetchSnapshot {
        path: destination,
        ..snapshot
    })
}

async fn populate_snapshot(
    temporary: &Path,
    timestamp: &str,
    gtfs_file: Option<&Path>,
    client: &TdxClient,
    sources: &FetchSources,
) -> Result<FetchSnapshot, FetchError> {
    let gtfs_path = temporary.join("gtfs.zip");
    let gtfs_source = if let Some(source_path) = gtfs_file {
        fs::copy(source_path, &gtfs_path)
            .map_err(|error| FetchError::Io(source_path.into(), error))?;
        FetchSource {
            source_id: "local-gtfs-file".to_owned(),
            url: None,
            sha256: hash_file(&gtfs_path)?,
            byte_size: file_size(&gtfs_path)?,
        }
    } else {
        let byte_size = client
            .download_url_to_path(&sources.gtfs_static_url, &gtfs_path)
            .await?;
        FetchSource {
            source_id: "tdx-gtfs-static-v3".to_owned(),
            url: Some(sources.gtfs_static_url.clone()),
            sha256: hash_file(&gtfs_path)?,
            byte_size,
        }
    };
    validate_gtfs_zip(&gtfs_path)?;

    let mut manifest = FetchManifest {
        manifest_version: 1,
        fetched_at: time::OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| timestamp.to_owned()),
        sources: BTreeMap::new(),
    };
    manifest.sources.insert("gtfs".to_owned(), gtfs_source);
    let mut bike_station_counts = BTreeMap::new();
    for (city, slug) in BIKE_CITIES {
        let url = station_url(&sources.bike_api_base_url, city)?;
        let (body, count) = fetch_stations(client, &url, city).await?;
        let source_name = format!("bike-stations-{slug}");
        let source = write_station_source(temporary, &source_name, &body, &url)?;
        manifest.sources.insert(source_name, source);
        bike_station_counts.insert((*city).to_owned(), count);
    }
    let manifest_path = temporary.join("manifest.json");
    let manifest_body =
        serde_json::to_vec_pretty(&manifest).map_err(|error| FetchError::InvalidBikeResponse {
            city: "manifest".to_owned(),
            detail: error.to_string(),
        })?;
    fs::write(&manifest_path, manifest_body)
        .map_err(|error| FetchError::Io(manifest_path, error))?;
    Ok(FetchSnapshot {
        path: temporary.to_owned(),
        manifest,
        bike_station_counts,
    })
}

async fn fetch_stations(
    client: &TdxClient,
    url: &str,
    city: &str,
) -> Result<(Vec<u8>, usize), FetchError> {
    let mut skip = 0usize;
    let mut stations = Vec::<Box<RawValue>>::new();
    let mut page_digests = HashSet::new();
    let mut station_ids = HashSet::new();
    loop {
        let skip_value = skip.to_string();
        let body = client
            .get_bytes(
                url,
                &[
                    ("$top", &client.settings.bike_catalog_page_size.to_string()),
                    ("$skip", skip_value.as_str()),
                    ("$orderby", BIKE_STATION_ORDER),
                    ("$format", "JSON"),
                ],
            )
            .await?;
        let page = serde_json::from_slice::<Vec<Box<RawValue>>>(&body).map_err(|error| {
            FetchError::InvalidBikeResponse {
                city: city.to_owned(),
                detail: error.to_string(),
            }
        })?;
        let digest: [u8; 32] = Sha256::digest(&body).into();
        if !page.is_empty() && !page_digests.insert(digest) {
            return Err(FetchError::InvalidBikeResponse {
                city: city.to_owned(),
                detail: "TDX repeated a non-empty station page".to_owned(),
            });
        }
        if page.is_empty() {
            break;
        }
        for station in &page {
            let identity =
                serde_json::from_str::<StationIdentity>(station.get()).map_err(|error| {
                    FetchError::InvalidBikeResponse {
                        city: city.to_owned(),
                        detail: format!("station identifier could not be read: {error}"),
                    }
                })?;
            let station_id = identity
                .station_uid
                .map(|station_id| station_id.trim().to_owned())
                .filter(|station_id| !station_id.is_empty())
                .ok_or_else(|| FetchError::InvalidBikeResponse {
                    city: city.to_owned(),
                    detail: "station record is missing StationUID".to_owned(),
                })?;
            if !station_ids.insert(station_id.clone()) {
                return Err(FetchError::InvalidBikeResponse {
                    city: city.to_owned(),
                    detail: format!("duplicate StationUID {station_id}"),
                });
            }
        }
        skip = skip.saturating_add(page.len());
        stations.extend(page);
    }
    let body = serde_json::to_vec(&stations).map_err(|error| FetchError::InvalidBikeResponse {
        city: city.to_owned(),
        detail: error.to_string(),
    })?;
    Ok((body, skip))
}

#[derive(Deserialize)]
struct StationIdentity {
    #[serde(rename = "StationUID")]
    station_uid: Option<String>,
}

fn write_station_source(
    root: &Path,
    source_name: &str,
    body: &[u8],
    url: &str,
) -> Result<FetchSource, FetchError> {
    let filename =
        source_file_name(source_name).ok_or_else(|| FetchError::InvalidBikeResponse {
            city: source_name.to_owned(),
            detail: "invalid station source name".to_owned(),
        })?;
    let path = root.join(filename);
    fs::write(&path, body).map_err(|error| FetchError::Io(path.clone(), error))?;
    source_metadata("tdx-bike-station-city", url.to_owned(), &path)
}

pub fn source_file_name(source_name: &str) -> Option<String> {
    if source_name == "gtfs" {
        return Some("gtfs.zip".to_owned());
    }
    BIKE_CITIES
        .iter()
        .find(|(_, slug)| source_name == format!("bike-stations-{slug}"))
        .map(|(_, slug)| format!("bike-stations-{slug}.json"))
}

pub fn bike_station_city(source_name: &str) -> Option<&'static str> {
    BIKE_CITIES
        .iter()
        .find(|(_, slug)| source_name == format!("bike-stations-{slug}"))
        .map(|(city, _)| *city)
}

fn station_url(base_url: &str, city: &str) -> Result<String, FetchError> {
    url::Url::parse(base_url)
        .and_then(|base| base.join(&format!("v2/Bike/Station/City/{city}")))
        .map(|url| url.to_string())
        .map_err(|error| FetchError::InvalidBikeResponse {
            city: city.to_owned(),
            detail: error.to_string(),
        })
}

fn source_metadata(source_id: &str, url: String, path: &Path) -> Result<FetchSource, FetchError> {
    Ok(FetchSource {
        source_id: source_id.to_owned(),
        url: Some(safe_source_url(&url)),
        sha256: hash_file(path)?,
        byte_size: file_size(path)?,
    })
}

fn safe_source_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return String::new();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn validate_gtfs_zip(path: &Path) -> Result<(), FetchError> {
    let file = File::open(path).map_err(|error| FetchError::Io(path.into(), error))?;
    let archive = ZipArchive::new(file).map_err(|_| FetchError::InvalidGtfsArchive)?;
    if archive.is_empty() {
        return Err(FetchError::InvalidGtfsArchive);
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, FetchError> {
    let mut file = File::open(path).map_err(|error| FetchError::Io(path.into(), error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| FetchError::Io(path.into(), error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn file_size(path: &Path) -> Result<u64, FetchError> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| FetchError::Io(path.into(), error))
}

pub fn timestamp_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::macros::format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .unwrap_or_else(|_| "19700101T000000Z".to_owned())
}

fn valid_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 16
        && bytes[0..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, Write},
        sync::{Arc, Mutex},
    };

    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{Request, Response, StatusCode},
        routing::any,
    };
    use zip::{ZipWriter, write::SimpleFileOptions};

    use crate::tdx::TdxCredentials;

    use super::*;

    #[test]
    fn station_source_rejects_an_invalid_name_before_writing() {
        let root = temporary_root("invalid-source");
        let error =
            write_station_source(&root, "invalid", b"[]", "https://example.com").unwrap_err();
        assert!(
            matches!(error, FetchError::InvalidBikeResponse { city, detail }
            if city == "invalid" && detail == "invalid station source name")
        );
        assert!(!root.exists());
    }

    #[test]
    fn station_source_preserves_file_write_errors() {
        let root = temporary_root("station-write-error");
        let error =
            write_station_source(&root, "bike-stations-taipei", b"[]", "https://example.com")
                .unwrap_err();
        assert!(matches!(error, FetchError::Io(path, error)
            if path == root.join("bike-stations-taipei.json") && error.kind() == std::io::ErrorKind::NotFound));
    }

    #[derive(Clone)]
    struct MockState {
        requests: Arc<Mutex<Vec<String>>>,
        gtfs_body: Vec<u8>,
        fail_new_taipei: bool,
        duplicate_taipei_station: bool,
    }

    fn mock_gtfs_zip() -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("stops.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"stop_id,stop_name\nstop,Stop\n").unwrap();
        zip.finish().unwrap().into_inner()
    }

    async fn mock_response(
        State(state): State<MockState>,
        request: Request<Body>,
    ) -> Response<Body> {
        let path = request
            .uri()
            .path_and_query()
            .map(|path| path.as_str().to_owned())
            .unwrap_or_default();
        let skip = request
            .uri()
            .query()
            .into_iter()
            .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
            .find(|(key, _)| key == "$skip")
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or_default();
        state.requests.lock().unwrap().push(path.clone());
        if path.starts_with("/token") {
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"access_token":"mock-token","expires_in":300}"#,
                ))
                .unwrap();
        }
        if path == "/gtfs" {
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/zip")
                .body(Body::from(state.gtfs_body))
                .unwrap();
        }
        if path.contains("NewTaipei") && state.fail_new_taipei && skip >= 2 {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Body::empty())
                .unwrap();
        }
        if path.starts_with("/basic/v2/Bike/Station/City/") {
            let body = match (path.contains("NewTaipei"), skip) {
                (false, _) if !path.starts_with("/basic/v2/Bike/Station/City/Taipei?") => "[]",
                (true, 0) => {
                    r#"[ {"StationUID":"NTP0001","StationID":"0001","StationName":{"Zh_tw":"New Taipei 1"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}} ]"#
                }
                (true, 1) => {
                    r#"[ {"StationUID":"NTP0002","StationID":"0002","StationName":{"Zh_tw":"New Taipei 2"},"StationPosition":{"PositionLat":25.1,"PositionLon":121.1}} ]"#
                }
                (false, 0) => {
                    r#"[ {"StationUID":"TPE0001","StationID":"0001","StationName":{"Zh_tw":"Taipei 1"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}} ]"#
                }
                (false, 1) if state.duplicate_taipei_station => {
                    r#"[ {"StationUID":"TPE0001","StationID":"0009","StationName":{"Zh_tw":"Taipei 9"},"StationPosition":{"PositionLat":25.9,"PositionLon":121.9}} ]"#
                }
                (false, 1) => {
                    r#"[ {"StationUID":"TPE0002","StationID":"0002","StationName":{"Zh_tw":"Taipei 2"},"StationPosition":{"PositionLat":25.1,"PositionLon":121.1}} ]"#
                }
                (_, _) => "[]",
            };
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    }

    async fn mock_client(
        fail_new_taipei: bool,
        duplicate_taipei_station: bool,
    ) -> (TdxClient, FetchSources, MockState) {
        let state = MockState {
            requests: Arc::new(Mutex::new(Vec::new())),
            gtfs_body: mock_gtfs_zip(),
            fail_new_taipei,
            duplicate_taipei_station,
        };
        let router = Router::new()
            .fallback(any(mock_response))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{address}");
        let client = TdxClient::new(
            TdxCredentials::from_client_credentials("client", "secret"),
            crate::tdx::TdxSettings {
                api_base_url: format!("{base_url}/basic/"),
                token_url: format!("{base_url}/token"),
                ..crate::config::ViaConfig::default().tdx
            },
        )
        .unwrap();
        let sources = FetchSources {
            gtfs_static_url: format!("{base_url}/gtfs"),
            bike_api_base_url: format!("{base_url}/basic/"),
        };
        (client, sources, state)
    }

    fn temporary_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "via-fetch-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn manifest_source_urls_drop_credentials_and_query_parameters() {
        assert_eq!(
            safe_source_url("https://user:secret@example.test/api/gtfs?access_token=hidden"),
            "https://example.test/api/gtfs"
        );
    }

    #[tokio::test]
    async fn local_gtfs_override_combines_city_pages_and_records_provenance() {
        let (client, sources, state) = mock_client(false, false).await;
        let root = temporary_root("local");
        fs::create_dir_all(&root).unwrap();
        let local_gtfs = root.join("source.zip");
        let expected_gtfs = mock_gtfs_zip();
        fs::write(&local_gtfs, &expected_gtfs).unwrap();

        let snapshot = fetch_snapshot_at(
            &root.join("fetch"),
            "20260928T041530Z",
            Some(&local_gtfs),
            &client,
            &sources,
        )
        .await
        .unwrap();

        assert_eq!(
            fs::read(snapshot.path.join("gtfs.zip")).unwrap(),
            expected_gtfs
        );
        assert!(
            !state
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|path| path == "/gtfs")
        );
        assert_eq!(snapshot.bike_station_counts["Taipei"], 2);
        assert_eq!(snapshot.bike_station_counts["NewTaipei"], 2);
        assert!(
            snapshot
                .bike_station_counts
                .values()
                .filter(|count| **count == 0)
                .count()
                > 0
        );
        assert_eq!(
            fs::read(snapshot.path.join("bike-stations-taipei.json")).unwrap(),
            br#"[{"StationUID":"TPE0001","StationID":"0001","StationName":{"Zh_tw":"Taipei 1"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}},{"StationUID":"TPE0002","StationID":"0002","StationName":{"Zh_tw":"Taipei 2"},"StationPosition":{"PositionLat":25.1,"PositionLon":121.1}}]"#
        );
        assert_eq!(
            fs::read(snapshot.path.join("bike-stations-new-taipei.json")).unwrap(),
            br#"[{"StationUID":"NTP0001","StationID":"0001","StationName":{"Zh_tw":"New Taipei 1"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}},{"StationUID":"NTP0002","StationID":"0002","StationName":{"Zh_tw":"New Taipei 2"},"StationPosition":{"PositionLat":25.1,"PositionLon":121.1}}]"#
        );
        let mut filenames = fs::read_dir(&snapshot.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        filenames.sort();
        let mut expected_filenames = BIKE_CITIES
            .iter()
            .map(|(_, slug)| format!("bike-stations-{slug}.json"))
            .collect::<Vec<_>>();
        expected_filenames.extend(["gtfs.zip".to_owned(), "manifest.json".to_owned()]);
        expected_filenames.sort();
        assert_eq!(filenames, expected_filenames);
        let requests = state.requests.lock().unwrap();
        for path in requests
            .iter()
            .filter(|path| path.starts_with("/basic/v2/Bike/Station/City/"))
        {
            let query = path.split_once('?').unwrap().1;
            assert!(
                url::form_urlencoded::parse(query.as_bytes())
                    .any(|(key, value)| key == "$orderby" && value == BIKE_STATION_ORDER)
            );
        }
        assert_eq!(
            requests
                .iter()
                .filter(|path| path.starts_with("/basic/v2/Bike/Station/City/Taipei?"))
                .count(),
            3
        );
        assert_eq!(
            requests
                .iter()
                .filter(|path| path.starts_with("/basic/v2/Bike/Station/City/NewTaipei?"))
                .count(),
            3
        );
        assert!(!requests.iter().any(|path| path == "/gtfs"));
        drop(requests);
        assert!(snapshot.path.join("manifest.json").is_file());
        let manifest = serde_json::from_slice::<FetchManifest>(
            &fs::read(snapshot.path.join("manifest.json")).unwrap(),
        )
        .unwrap();
        let mut expected_sources = BIKE_CITIES
            .iter()
            .map(|(_, slug)| format!("bike-stations-{slug}"))
            .collect::<Vec<_>>();
        expected_sources.push("gtfs".to_owned());
        expected_sources.sort();
        assert_eq!(
            manifest.sources.keys().cloned().collect::<Vec<_>>(),
            expected_sources
        );
        for (name, source) in manifest.sources {
            let filename = source_file_name(&name).unwrap();
            let path = snapshot.path.join(filename);
            assert_eq!(source.byte_size, fs::metadata(&path).unwrap().len());
            assert_eq!(source.sha256, hash_file(&path).unwrap());
        }
        assert!(source_file_name("bike-stations-taipei-page-0002").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn gtfs_http_download_preserves_the_response_bytes() {
        let (client, sources, _) = mock_client(false, false).await;
        let root = temporary_root("http");
        let expected = mock_gtfs_zip();
        let snapshot = fetch_snapshot_at(&root, "20260928T041531Z", None, &client, &sources)
            .await
            .unwrap();
        assert_eq!(fs::read(snapshot.path.join("gtfs.zip")).unwrap(), expected);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_source_does_not_publish_a_complete_snapshot() {
        let (client, sources, state) = mock_client(true, false).await;
        let root = temporary_root("failure");
        let local_gtfs = root.join("source.zip");
        fs::create_dir_all(&root).unwrap();
        fs::write(&local_gtfs, mock_gtfs_zip()).unwrap();

        assert!(
            fetch_snapshot_at(
                &root.join("fetch"),
                "20260928T041532Z",
                Some(&local_gtfs),
                &client,
                &sources,
            )
            .await
            .is_err()
        );
        let requests = state.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|path| path.starts_with("/basic/v2/Bike/Station/City/NewTaipei?"))
                .count(),
            3
        );
        assert!(!requests.iter().any(|path| path == "/gtfs"));
        drop(requests);
        let fetch_root = root.join("fetch");
        assert_eq!(fs::read_dir(&fetch_root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn duplicate_station_identifiers_across_pages_reject_the_snapshot() {
        let (client, sources, _) = mock_client(false, true).await;
        let root = temporary_root("duplicate-stations");
        fs::create_dir_all(&root).unwrap();
        let local_gtfs = root.join("source.zip");
        fs::write(&local_gtfs, mock_gtfs_zip()).unwrap();

        let error = fetch_snapshot_at(
            &root.join("fetch"),
            "20260928T041533Z",
            Some(&local_gtfs),
            &client,
            &sources,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            FetchError::InvalidBikeResponse { city, detail }
                if city == "Taipei" && detail.contains("duplicate StationUID TPE0001")
        ));
        assert_eq!(fs::read_dir(root.join("fetch")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
