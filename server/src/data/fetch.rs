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

use crate::tdx::{TdxClient, TdxClientError, TdxCredentials, TdxCredentialsError};

pub const TDX_GTFS_STATIC_URL: &str = "https://tdx.transportdata.tw/api/gtfs/V3/Map/GTFS/Static";
const BIKE_PAGE_SIZE: &str = "1000";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchSource {
    pub source_id: String,
    pub url: Option<String>,
    pub sha256: String,
    pub byte_size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    #[error("TDX credentials are unavailable: {0}")]
    Credentials(#[from] TdxCredentialsError),
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

pub async fn fetch_snapshot(gtfs_file: Option<&Path>) -> Result<FetchSnapshot, FetchError> {
    let credentials = TdxCredentials::from_env()?;
    let base_url = std::env::var("VIA_TDX_BASE_URL")
        .unwrap_or_else(|_| "https://tdx.transportdata.tw/api/basic/".to_owned());
    let client = TdxClient::with_api_base_url(credentials, &base_url)?;
    let snapshot_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/fetch");
    fetch_snapshot_at(
        &snapshot_root,
        &timestamp_now(),
        gtfs_file,
        &client,
        &FetchSources::production(base_url),
    )
    .await
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

    let taipei_url = station_url(&sources.bike_api_base_url, "Taipei")?;
    let new_taipei_url = station_url(&sources.bike_api_base_url, "NewTaipei")?;
    let (taipei_pages, taipei_count) = fetch_station_pages(client, &taipei_url, "Taipei").await?;
    let (new_taipei_pages, new_taipei_count) =
        fetch_station_pages(client, &new_taipei_url, "NewTaipei").await?;

    let mut manifest = FetchManifest {
        manifest_version: 1,
        fetched_at: time::OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| timestamp.to_owned()),
        sources: BTreeMap::new(),
    };
    manifest.sources.insert("gtfs".to_owned(), gtfs_source);
    write_station_pages(
        temporary,
        "bike-stations-taipei",
        &taipei_pages,
        &taipei_url,
        &mut manifest.sources,
    )?;
    write_station_pages(
        temporary,
        "bike-stations-new-taipei",
        &new_taipei_pages,
        &new_taipei_url,
        &mut manifest.sources,
    )?;
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
        bike_station_counts: BTreeMap::from([
            ("Taipei".to_owned(), taipei_count),
            ("NewTaipei".to_owned(), new_taipei_count),
        ]),
    })
}

async fn fetch_station_pages(
    client: &TdxClient,
    url: &str,
    city: &str,
) -> Result<(Vec<Vec<u8>>, usize), FetchError> {
    let mut skip = 0usize;
    let mut pages = Vec::new();
    let mut page_digests = HashSet::new();
    loop {
        let skip_value = skip.to_string();
        let body = client
            .get_bytes(
                url,
                &[
                    ("$top", BIKE_PAGE_SIZE),
                    ("$skip", skip_value.as_str()),
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
        pages.push(body.to_vec());
        if page.is_empty() {
            if skip == 0 {
                return Err(FetchError::InvalidBikeResponse {
                    city: city.to_owned(),
                    detail: "response contained no station records".to_owned(),
                });
            }
            break;
        }
        skip = skip.saturating_add(page.len());
    }
    Ok((pages, skip))
}

fn write_station_pages(
    root: &Path,
    source_prefix: &str,
    pages: &[Vec<u8>],
    url: &str,
    manifest_sources: &mut BTreeMap<String, FetchSource>,
) -> Result<(), FetchError> {
    for (index, body) in pages.iter().enumerate() {
        let source_name = if index == 0 {
            source_prefix.to_owned()
        } else {
            format!("{source_prefix}-page-{:04}", index + 1)
        };
        let filename =
            source_file_name(&source_name).ok_or_else(|| FetchError::InvalidBikeResponse {
                city: source_prefix.to_owned(),
                detail: "invalid station page name".to_owned(),
            })?;
        let path = root.join(filename);
        fs::write(&path, body).map_err(|error| FetchError::Io(path.clone(), error))?;
        manifest_sources.insert(
            source_name,
            source_metadata("tdx-bike-station-city", url.to_owned(), &path)?,
        );
    }
    Ok(())
}

pub fn source_file_name(source_name: &str) -> Option<String> {
    match source_name {
        "gtfs" => Some("gtfs.zip".to_owned()),
        "bike-stations-taipei" | "bike-stations-new-taipei" => Some(format!("{source_name}.json")),
        _ => ["bike-stations-taipei", "bike-stations-new-taipei"]
            .into_iter()
            .find_map(|prefix| {
                let suffix = source_name.strip_prefix(&format!("{prefix}-page-"))?;
                (suffix.len() == 4 && suffix.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| format!("{source_name}.json"))
            }),
    }
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

    use super::*;

    #[derive(Clone)]
    struct MockState {
        requests: Arc<Mutex<Vec<String>>>,
        gtfs_body: Vec<u8>,
        fail_new_taipei: bool,
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
        if path.contains("NewTaipei") && state.fail_new_taipei {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Body::empty())
                .unwrap();
        }
        if path.contains("Taipei") {
            let body = if skip > 0 {
                "[]"
            } else if path.contains("NewTaipei") {
                r#"[ {"StationUID":"NTP0001","StationID":"0001","StationName":{"Zh_tw":"New Taipei"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}} ]"#
            } else {
                r#"[ {"StationUID":"TPE0001","StationID":"0001","StationName":{"Zh_tw":"Taipei"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}} ]"#
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

    async fn mock_client(fail_new_taipei: bool) -> (TdxClient, FetchSources, MockState) {
        let state = MockState {
            requests: Arc::new(Mutex::new(Vec::new())),
            gtfs_body: mock_gtfs_zip(),
            fail_new_taipei,
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
        let client = TdxClient::with_urls(
            TdxCredentials::from_client_credentials("client", "secret"),
            &format!("{base_url}/basic/"),
            &format!("{base_url}/token"),
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
    async fn local_gtfs_override_copies_bytes_and_skips_the_gtfs_request() {
        let (client, sources, state) = mock_client(false).await;
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
        assert_eq!(snapshot.bike_station_counts["Taipei"], 1);
        assert_eq!(snapshot.bike_station_counts["NewTaipei"], 1);
        assert_eq!(
            fs::read(snapshot.path.join("bike-stations-taipei.json")).unwrap(),
            br#"[ {"StationUID":"TPE0001","StationID":"0001","StationName":{"Zh_tw":"Taipei"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}} ]"#
        );
        assert!(
            snapshot
                .path
                .join("bike-stations-new-taipei.json")
                .is_file()
        );
        assert_eq!(
            fs::read(snapshot.path.join("bike-stations-new-taipei.json")).unwrap(),
            br#"[ {"StationUID":"NTP0001","StationID":"0001","StationName":{"Zh_tw":"New Taipei"},"StationPosition":{"PositionLat":25.0,"PositionLon":121.0}} ]"#
        );
        assert_eq!(
            fs::read(snapshot.path.join("bike-stations-taipei-page-0002.json")).unwrap(),
            b"[]"
        );
        assert_eq!(
            fs::read(
                snapshot
                    .path
                    .join("bike-stations-new-taipei-page-0002.json")
            )
            .unwrap(),
            b"[]"
        );
        assert!(snapshot.path.join("manifest.json").is_file());
        let manifest = serde_json::from_slice::<FetchManifest>(
            &fs::read(snapshot.path.join("manifest.json")).unwrap(),
        )
        .unwrap();
        for (name, source) in manifest.sources {
            let filename = source_file_name(&name).unwrap();
            let path = snapshot.path.join(filename);
            assert_eq!(source.byte_size, fs::metadata(&path).unwrap().len());
            assert_eq!(source.sha256, hash_file(&path).unwrap());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn gtfs_http_download_preserves_the_response_bytes() {
        let (client, sources, _) = mock_client(false).await;
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
        let (client, sources, _) = mock_client(true).await;
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
        let fetch_root = root.join("fetch");
        assert_eq!(fs::read_dir(&fetch_root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
