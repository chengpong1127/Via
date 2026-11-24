use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    future::Future,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use zip::ZipArchive;

use crate::data::DataError;
use crate::data::bus_routes::{BUS_ROUTE_CITIES, BUS_ROUTE_FIELDS, BusRoute};
use crate::data::via_static::ViaStaticSource;
use crate::tdx::{BIKE_CITIES, TdxClient, TdxClientError, TdxFetchSettings};

pub const TDX_GTFS_STATIC_URL: &str = "https://tdx.transportdata.tw/api/gtfs/V3/Map/GTFS/Static";
const BIKE_STATION_ORDER: &str = "StationUID asc";
pub const GTFS_SOURCE_NAME: &str = "gtfs";
pub const GTFS_FILE_NAME: &str = "gtfs.zip";
pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const MANIFEST_VERSION: u32 = 1;

pub fn bike_station_source_name(slug: &str) -> String {
    format!("bike-stations-{slug}")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchManifest {
    pub manifest_version: u32,
    pub fetched_at: String,
    pub sources: BTreeMap<String, ViaStaticSource>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reused_sources: BTreeMap<String, String>,
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
    #[error("TDX Bus Route response for {city} was invalid: {detail}")]
    InvalidBusRoutes { city: String, detail: String },
    #[error("downloaded GTFS data is not a valid ZIP archive")]
    InvalidGtfsArchive,
    #[error("fetch snapshot timestamp {0} is invalid")]
    InvalidTimestamp(String),
    #[error("fetch snapshot already exists at {0}")]
    SnapshotExists(PathBuf),
    #[error("no previous snapshot copy of {0} is available to reuse")]
    MissingPreviousSource(String),
    #[error("fetch manifest could not be serialized: {0}")]
    InvalidManifest(String),
}

#[derive(Clone, Debug)]
pub struct FetchSources {
    pub gtfs_static_url: String,
    pub bike_api_base_url: String,
    pub policy: FetchPolicy,
}

#[derive(Clone, Debug)]
pub struct FetchPolicy {
    pub request_interval: Duration,
    pub max_attempts: u32,
    pub retry_backoff: Duration,
    pub gtfs_only: bool,
}

impl FetchPolicy {
    pub fn from_settings(settings: &TdxFetchSettings, gtfs_only: bool) -> Self {
        Self {
            request_interval: Duration::from_secs(60)
                .div_f64(f64::from(settings.requests_per_minute.max(1))),
            max_attempts: settings.max_attempts.max(1),
            retry_backoff: Duration::from_secs(settings.retry_backoff_seconds),
            gtfs_only,
        }
    }
}

impl FetchSources {
    pub fn production(bike_api_base_url: impl Into<String>, policy: FetchPolicy) -> Self {
        Self {
            gtfs_static_url: TDX_GTFS_STATIC_URL.to_owned(),
            bike_api_base_url: bike_api_base_url.into(),
            policy,
        }
    }
}

struct RequestPacer {
    interval: Duration,
    last: Option<tokio::time::Instant>,
}

impl RequestPacer {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    async fn wait(&mut self) {
        if let Some(last) = self.last {
            tokio::time::sleep_until(last + self.interval).await;
        }
        self.last = Some(tokio::time::Instant::now());
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
    let previous = PreviousSnapshot::latest_in(root);
    let result = populate_snapshot(
        &temporary,
        timestamp,
        gtfs_file,
        previous.as_ref(),
        client,
        sources,
    )
    .await;
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
    previous: Option<&PreviousSnapshot>,
    client: &TdxClient,
    sources: &FetchSources,
) -> Result<FetchSnapshot, FetchError> {
    let policy = &sources.policy;
    let mut pacer = RequestPacer::new(policy.request_interval);
    let gtfs_path = temporary.join(GTFS_FILE_NAME);
    let gtfs_source = if let Some(source_path) = gtfs_file {
        fs::copy(source_path, &gtfs_path)
            .map_err(|error| FetchError::Io(source_path.into(), error))?;
        ViaStaticSource {
            source_id: "local-gtfs-file".to_owned(),
            source_url: None,
            sha256: hash_file(&gtfs_path)?,
            byte_size: file_size(&gtfs_path)?,
        }
    } else {
        let partial_path = temporary.join(format!("{GTFS_FILE_NAME}.part"));
        let byte_size = with_retries(policy, &mut pacer, || {
            client.download_to_path(&sources.gtfs_static_url, &partial_path, true)
        })
        .await?;
        validate_gtfs_zip(&partial_path)?;
        fs::rename(&partial_path, &gtfs_path)
            .map_err(|error| FetchError::Io(partial_path.clone(), error))?;
        ViaStaticSource {
            source_id: "tdx-gtfs-static-v3".to_owned(),
            source_url: Some(sources.gtfs_static_url.clone()),
            sha256: hash_file(&gtfs_path)?,
            byte_size,
        }
    };
    validate_gtfs_zip(&gtfs_path)?;

    let mut manifest = FetchManifest {
        manifest_version: MANIFEST_VERSION,
        fetched_at: time::OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| timestamp.to_owned()),
        sources: BTreeMap::new(),
        reused_sources: BTreeMap::new(),
    };
    manifest
        .sources
        .insert(GTFS_SOURCE_NAME.to_owned(), gtfs_source);
    let mut bike_station_counts = BTreeMap::new();
    for (city, slug) in BIKE_CITIES {
        let source_name = bike_station_source_name(slug);
        let url = station_url(&sources.bike_api_base_url, city)?;
        let fetched = if policy.gtfs_only {
            None
        } else {
            match fetch_stations(client, policy, &mut pacer, &url, city).await {
                Ok((body, count)) => {
                    let source = write_station_source(temporary, &source_name, &body, &url)?;
                    Some((source, count))
                }
                Err(error @ FetchError::Tdx(_)) => {
                    eprintln!("{source_name} fetch failed: {error}");
                    None
                }
                Err(error) => return Err(error),
            }
        };
        let (source, count) = match fetched {
            Some(fetched) => fetched,
            None => {
                let source = reuse_previous(previous, &source_name, temporary, &mut manifest)?;
                let count = station_count(&temporary.join(format!("{source_name}.json")), city)?;
                (source, count)
            }
        };
        manifest.sources.insert(source_name, source);
        bike_station_counts.insert((*city).to_owned(), count);
    }
    for (city, slug) in BUS_ROUTE_CITIES {
        let source_name = format!("bus-routes-{slug}");
        let url = bus_route_url(&sources.bike_api_base_url, city)?;
        let fetched = if policy.gtfs_only {
            None
        } else {
            match fetch_bus_routes(client, policy, &mut pacer, &url, city).await {
                Ok(body) => {
                    let path = temporary.join(format!("{source_name}.json"));
                    fs::write(&path, &body).map_err(|error| FetchError::Io(path.clone(), error))?;
                    Some(source_metadata("tdx-bus-route-city", url, &path)?)
                }
                Err(error @ FetchError::Tdx(_)) => {
                    eprintln!("{source_name} fetch failed: {error}");
                    None
                }
                Err(error) => return Err(error),
            }
        };
        let source = match fetched {
            Some(source) => source,
            None => reuse_previous(previous, &source_name, temporary, &mut manifest)?,
        };
        manifest.sources.insert(source_name, source);
    }
    let manifest_path = temporary.join(MANIFEST_FILE_NAME);
    let manifest_body = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| FetchError::InvalidManifest(error.to_string()))?;
    fs::write(&manifest_path, manifest_body)
        .map_err(|error| FetchError::Io(manifest_path, error))?;
    Ok(FetchSnapshot {
        path: temporary.to_owned(),
        manifest,
        bike_station_counts,
    })
}

async fn fetch_bus_routes(
    client: &TdxClient,
    policy: &FetchPolicy,
    pacer: &mut RequestPacer,
    url: &str,
    city: &str,
) -> Result<bytes::Bytes, FetchError> {
    let body = with_retries(policy, pacer, || {
        client.fetch_bytes(url, &[("$select", BUS_ROUTE_FIELDS), ("$format", "JSON")])
    })
    .await?;
    serde_json::from_slice::<Vec<BusRoute>>(&body).map_err(|error| {
        FetchError::InvalidBusRoutes {
            city: city.to_owned(),
            detail: error.to_string(),
        }
    })?;
    Ok(body)
}

async fn with_retries<T, Fut>(
    policy: &FetchPolicy,
    pacer: &mut RequestPacer,
    mut attempt: impl FnMut() -> Fut,
) -> Result<T, TdxClientError>
where
    Fut: Future<Output = Result<T, TdxClientError>>,
{
    let mut attempts = 1u32;
    loop {
        pacer.wait().await;
        match attempt().await {
            Err(error) if error.is_transient() && attempts < policy.max_attempts => {
                let delay = match &error {
                    TdxClientError::RateLimitedWithRetryAfter(delay) => *delay,
                    _ => policy.retry_backoff * 2u32.saturating_pow(attempts - 1),
                };
                eprintln!("TDX request failed ({error}); retry {attempts} in {delay:?}");
                tokio::time::sleep(delay).await;
                attempts += 1;
            }
            outcome => return outcome,
        }
    }
}

struct PreviousSnapshot {
    path: PathBuf,
    manifest: FetchManifest,
}

impl PreviousSnapshot {
    fn latest_in(root: &Path) -> Option<Self> {
        let path = fs::canonicalize(root.join("latest")).ok()?;
        let body = fs::read(path.join(MANIFEST_FILE_NAME)).ok()?;
        let manifest = serde_json::from_slice::<FetchManifest>(&body).ok()?;
        Some(Self { path, manifest })
    }
}

fn reuse_previous(
    previous: Option<&PreviousSnapshot>,
    source_name: &str,
    temporary: &Path,
    manifest: &mut FetchManifest,
) -> Result<ViaStaticSource, FetchError> {
    let missing = || FetchError::MissingPreviousSource(source_name.to_owned());
    let previous = previous.ok_or_else(missing)?;
    let source = previous
        .manifest
        .sources
        .get(source_name)
        .ok_or_else(missing)?;
    let filename = source_file_name(source_name).ok_or_else(missing)?;
    let from = previous.path.join(&filename);
    if hash_file(&from)? != source.sha256 {
        return Err(missing());
    }
    let to = temporary.join(&filename);
    fs::copy(&from, &to).map_err(|error| FetchError::Io(from, error))?;
    manifest
        .reused_sources
        .insert(source_name.to_owned(), previous.manifest.fetched_at.clone());
    Ok(source.clone())
}

fn station_count(path: &Path, city: &str) -> Result<usize, FetchError> {
    let body = fs::read(path).map_err(|error| FetchError::Io(path.into(), error))?;
    serde_json::from_slice::<Vec<&RawValue>>(&body)
        .map(|stations| stations.len())
        .map_err(|error| FetchError::InvalidBikeResponse {
            city: city.to_owned(),
            detail: error.to_string(),
        })
}

async fn fetch_stations(
    client: &TdxClient,
    policy: &FetchPolicy,
    pacer: &mut RequestPacer,
    url: &str,
    city: &str,
) -> Result<(Vec<u8>, usize), FetchError> {
    let invalid = |detail: String| FetchError::InvalidBikeResponse {
        city: city.to_owned(),
        detail,
    };
    let mut skip = 0usize;
    let mut stations = Vec::<Box<RawValue>>::new();
    let mut page_digests = HashSet::new();
    let mut station_ids = HashSet::new();
    loop {
        let skip_value = skip.to_string();
        let page_size = client.settings().bike_catalog_page_size.to_string();
        let query = [
            ("$top", page_size.as_str()),
            ("$skip", skip_value.as_str()),
            ("$orderby", BIKE_STATION_ORDER),
            ("$format", "JSON"),
        ];
        let body = with_retries(policy, pacer, || client.fetch_bytes(url, &query)).await?;
        let page = serde_json::from_slice::<Vec<Box<RawValue>>>(&body)
            .map_err(|error| invalid(error.to_string()))?;
        let digest: [u8; 32] = Sha256::digest(&body).into();
        if !page.is_empty() && !page_digests.insert(digest) {
            return Err(invalid("TDX repeated a non-empty station page".to_owned()));
        }
        if page.is_empty() {
            break;
        }
        for station in &page {
            let identity =
                serde_json::from_str::<StationIdentity>(station.get()).map_err(|error| {
                    invalid(format!("station identifier could not be read: {error}"))
                })?;
            let station_id = identity
                .station_uid
                .map(|station_id| station_id.trim().to_owned())
                .filter(|station_id| !station_id.is_empty())
                .ok_or_else(|| invalid("station record is missing StationUID".to_owned()))?;
            if !station_ids.insert(station_id.clone()) {
                return Err(invalid(format!("duplicate StationUID {station_id}")));
            }
        }
        skip = skip.saturating_add(page.len());
        stations.extend(page);
    }
    let body = serde_json::to_vec(&stations).map_err(|error| invalid(error.to_string()))?;
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
) -> Result<ViaStaticSource, FetchError> {
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
    if source_name == GTFS_SOURCE_NAME {
        return Some(GTFS_FILE_NAME.to_owned());
    }
    bike_city_source(source_name)
        .map(|(_, slug)| format!("{}.json", bike_station_source_name(slug)))
        .or_else(|| is_bus_route_source(source_name).then(|| format!("{source_name}.json")))
}

pub fn is_bus_route_source(source_name: &str) -> bool {
    BUS_ROUTE_CITIES
        .iter()
        .any(|(_, slug)| source_name == format!("bus-routes-{slug}"))
}

pub fn bike_station_city(source_name: &str) -> Option<&'static str> {
    bike_city_source(source_name).map(|(city, _)| *city)
}

fn bike_city_source(source_name: &str) -> Option<&'static (&'static str, &'static str)> {
    BIKE_CITIES
        .iter()
        .find(|(_, slug)| source_name == bike_station_source_name(slug))
}

pub(crate) struct SnapshotInputs {
    pub(crate) manifest: FetchManifest,
    pub(crate) gtfs_path: PathBuf,
    pub(crate) bike_station_catalogs: Vec<(String, String)>,
    pub(crate) bus_routes: Vec<BusRoute>,
}

pub(crate) fn read_snapshot(input: &Path) -> Result<SnapshotInputs, DataError> {
    let required_files = [GTFS_FILE_NAME, MANIFEST_FILE_NAME]
        .into_iter()
        .collect::<HashSet<_>>();
    let mut actual_files = HashSet::new();
    for entry in fs::read_dir(input).map_err(|error| DataError::Io(input.into(), error))? {
        let entry = entry.map_err(|error| DataError::Io(input.into(), error))?;
        let filename = entry.file_name().into_string().map_err(|_| {
            DataError::InvalidFetchSnapshot("snapshot contains a non-UTF-8 filename".to_owned())
        })?;
        if !required_files.contains(filename.as_str())
            && !BIKE_CITIES
                .iter()
                .any(|(_, slug)| filename == format!("{}.json", bike_station_source_name(slug)))
            && !BUS_ROUTE_CITIES
                .iter()
                .any(|(_, slug)| filename == format!("bus-routes-{slug}.json"))
        {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "untracked source file {filename}"
            )));
        }
        if !entry
            .file_type()
            .map_err(|error| DataError::Io(input.into(), error))?
            .is_file()
        {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "snapshot entry {filename} is not a regular file"
            )));
        }
        actual_files.insert(filename);
    }
    if !required_files
        .iter()
        .all(|filename| actual_files.contains(*filename))
    {
        let missing_files = required_files
            .iter()
            .filter(|filename| !actual_files.contains(**filename))
            .copied()
            .collect::<Vec<_>>();
        return Err(DataError::InvalidFetchSnapshot(format!(
            "snapshot is missing required files: {}",
            missing_files.join(", ")
        )));
    }

    let manifest_path = input.join(MANIFEST_FILE_NAME);
    let manifest_body = fs::read_to_string(&manifest_path)
        .map_err(|error| DataError::Io(manifest_path.clone(), error))?;
    let manifest = serde_json::from_str::<FetchManifest>(&manifest_body)
        .map_err(|error| DataError::InvalidFetchSnapshot(error.to_string()))?;
    if manifest.manifest_version != MANIFEST_VERSION
        || time::OffsetDateTime::parse(&manifest.fetched_at, &Rfc3339).is_err()
    {
        return Err(DataError::InvalidFetchSnapshot(
            "unsupported manifest version or invalid fetch timestamp".to_owned(),
        ));
    }
    if !manifest.sources.contains_key(GTFS_SOURCE_NAME) {
        return Err(DataError::InvalidFetchSnapshot(
            "manifest must contain a gtfs source".to_owned(),
        ));
    }
    let expected_files = manifest
        .sources
        .keys()
        .filter_map(|name| source_file_name(name))
        .chain(std::iter::once(MANIFEST_FILE_NAME.to_owned()))
        .collect::<HashSet<_>>();
    if expected_files != actual_files {
        return Err(DataError::InvalidFetchSnapshot(
            "snapshot files do not match manifest sources".to_owned(),
        ));
    }
    let mut bike_station_catalogs = Vec::new();
    let mut bus_routes = Vec::new();
    for (source_name, source) in &manifest.sources {
        if source.source_id.trim().is_empty() || !source.has_valid_sha256() {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "{source_name} has invalid provenance metadata"
            )));
        }
        let filename = source_file_name(source_name).ok_or_else(|| {
            DataError::InvalidFetchSnapshot(format!("unknown source {source_name}"))
        })?;
        let source_path = input.join(filename);
        let metadata = fs::metadata(&source_path)
            .map_err(|error| DataError::Io(source_path.clone(), error))?;
        if source.byte_size != metadata.len()
            || source.sha256
                != sha256_file(&source_path)
                    .map_err(|error| DataError::Io(source_path.clone(), error))?
        {
            return Err(DataError::InvalidFetchSnapshot(format!(
                "{source_name} does not match its fetch provenance"
            )));
        }
        if let Some(city) = bike_station_city(source_name) {
            bike_station_catalogs.push((
                city.to_owned(),
                fs::read_to_string(&source_path)
                    .map_err(|error| DataError::Io(source_path.clone(), error))?,
            ));
        }
        if is_bus_route_source(source_name) {
            let body = fs::read(&source_path)
                .map_err(|error| DataError::Io(source_path.clone(), error))?;
            bus_routes.extend(
                serde_json::from_slice::<Vec<BusRoute>>(&body).map_err(|error| {
                    DataError::InvalidFetchSnapshot(format!("{source_name}: {error}"))
                })?,
            );
        }
    }

    Ok(SnapshotInputs {
        gtfs_path: input.join(GTFS_FILE_NAME),
        manifest,
        bike_station_catalogs,
        bus_routes,
    })
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

fn bus_route_url(base_url: &str, city: &str) -> Result<String, FetchError> {
    url::Url::parse(base_url)
        .and_then(|base| base.join(&format!("v2/Bus/Route/City/{city}")))
        .map(|url| url.to_string())
        .map_err(|error| FetchError::InvalidBusRoutes {
            city: city.to_owned(),
            detail: error.to_string(),
        })
}

fn source_metadata(
    source_id: &str,
    url: String,
    path: &Path,
) -> Result<ViaStaticSource, FetchError> {
    Ok(ViaStaticSource {
        source_id: source_id.to_owned(),
        source_url: Some(safe_source_url(&url)),
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
    sha256_file(path).map_err(|error| FetchError::Io(path.into(), error))
}

pub(crate) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
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
        request_times: Arc<Mutex<Vec<std::time::Instant>>>,
        scripted_failures: Arc<Mutex<Vec<ScriptedFailure>>>,
        gtfs_body: Vec<u8>,
        fail_new_taipei: Arc<std::sync::atomic::AtomicBool>,
        duplicate_taipei_station: bool,
    }

    struct ScriptedFailure {
        path_prefix: &'static str,
        status: u16,
        retry_after_seconds: Option<u64>,
    }

    impl MockState {
        fn script_failure(
            &self,
            path_prefix: &'static str,
            status: u16,
            retry_after_seconds: Option<u64>,
        ) {
            self.scripted_failures
                .lock()
                .unwrap()
                .push(ScriptedFailure {
                    path_prefix,
                    status,
                    retry_after_seconds,
                });
        }

        fn request_count(&self, path_prefix: &str) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|path| path.starts_with(path_prefix))
                .count()
        }
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
        state
            .request_times
            .lock()
            .unwrap()
            .push(std::time::Instant::now());
        if !path.starts_with("/token") {
            let mut scripted = state.scripted_failures.lock().unwrap();
            if let Some(index) = scripted
                .iter()
                .position(|failure| path.starts_with(failure.path_prefix))
            {
                let failure = scripted.remove(index);
                let mut response = Response::builder().status(failure.status);
                if let Some(seconds) = failure.retry_after_seconds {
                    response = response.header("retry-after", seconds.to_string());
                }
                return response.body(Body::empty()).unwrap();
            }
        }
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
        if path.contains("NewTaipei")
            && state
                .fail_new_taipei
                .load(std::sync::atomic::Ordering::SeqCst)
            && skip >= 2
        {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Body::empty())
                .unwrap();
        }
        if path.starts_with("/basic/v2/Bus/Route/City/") {
            let body = if path.starts_with("/basic/v2/Bus/Route/City/NewTaipei?") {
                r#"[{"RouteUID":"NWT16517","SubRoutes":[{"SubRouteUID":"NWT157552"}]}]"#
            } else {
                "[]"
            };
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(body))
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
            request_times: Arc::new(Mutex::new(Vec::new())),
            scripted_failures: Arc::new(Mutex::new(Vec::new())),
            gtfs_body: mock_gtfs_zip(),
            fail_new_taipei: Arc::new(std::sync::atomic::AtomicBool::new(fail_new_taipei)),
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
            policy: FetchPolicy {
                request_interval: Duration::ZERO,
                max_attempts: 3,
                retry_backoff: Duration::ZERO,
                gtfs_only: false,
            },
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
        expected_filenames.extend(
            BUS_ROUTE_CITIES
                .iter()
                .map(|(_, slug)| format!("bus-routes-{slug}.json")),
        );
        expected_filenames.extend(["gtfs.zip".to_owned(), "manifest.json".to_owned()]);
        expected_filenames.sort();
        assert_eq!(filenames, expected_filenames);
        assert_eq!(
            fs::read(snapshot.path.join("bus-routes-new-taipei.json")).unwrap(),
            br#"[{"RouteUID":"NWT16517","SubRoutes":[{"SubRouteUID":"NWT157552"}]}]"#
        );
        assert_eq!(
            snapshot.manifest.sources["bus-routes-taipei"]
                .source_url
                .as_deref(),
            Some(format!("{}v2/Bus/Route/City/Taipei", sources.bike_api_base_url).as_str())
        );
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
            .chain(
                BUS_ROUTE_CITIES
                    .iter()
                    .map(|(_, slug)| format!("bus-routes-{slug}")),
            )
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
        assert_eq!(
            state.request_count("/basic/v2/Bike/Station/City/NewTaipei?"),
            5
        );
        assert_eq!(state.request_count("/gtfs"), 0);
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

    async fn fetch_with_local_gtfs(
        root: &Path,
        timestamp: &str,
        client: &TdxClient,
        sources: &FetchSources,
    ) -> Result<FetchSnapshot, FetchError> {
        let local_gtfs = root.join("source.zip");
        fs::create_dir_all(root).unwrap();
        fs::write(&local_gtfs, mock_gtfs_zip()).unwrap();
        fetch_snapshot_at(
            &root.join("fetch"),
            timestamp,
            Some(&local_gtfs),
            client,
            sources,
        )
        .await
    }

    fn promote_latest(root: &Path, timestamp: &str) {
        std::os::unix::fs::symlink(timestamp, root.join("fetch/latest")).unwrap();
    }

    #[tokio::test]
    async fn a_transient_failure_is_retried_and_the_snapshot_completes() {
        let (client, sources, state) = mock_client(false, false).await;
        state.script_failure("/basic/v2/Bike/Station/City/Taipei?", 503, None);
        let root = temporary_root("transient");

        let snapshot = fetch_with_local_gtfs(&root, "20260928T041540Z", &client, &sources)
            .await
            .unwrap();

        assert_eq!(snapshot.bike_station_counts["Taipei"], 2);
        assert!(snapshot.manifest.reused_sources.is_empty());
        assert_eq!(
            state.request_count("/basic/v2/Bike/Station/City/Taipei?"),
            4
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rate_limit_responses_wait_for_retry_after() {
        let (client, sources, state) = mock_client(false, false).await;
        state.script_failure("/basic/v2/Bike/Station/City/Taipei?", 429, Some(1));
        let root = temporary_root("retry-after");
        let started = std::time::Instant::now();

        fetch_with_local_gtfs(&root, "20260928T041541Z", &client, &sources)
            .await
            .unwrap();

        assert!(started.elapsed() >= Duration::from_secs(1));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn every_tdx_call_waits_for_the_request_budget() {
        let (client, mut sources, state) = mock_client(false, false).await;
        sources.policy.request_interval = Duration::from_millis(60);
        state.script_failure("/basic/v2/Bike/Station/City/Taipei?", 503, None);
        let root = temporary_root("pacing");

        fetch_with_local_gtfs(&root, "20260928T041542Z", &client, &sources)
            .await
            .unwrap();

        let requests = state.requests.lock().unwrap();
        let times = state.request_times.lock().unwrap();
        let api_times = requests
            .iter()
            .zip(times.iter())
            .filter(|(path, _)| !path.starts_with("/token"))
            .map(|(_, time)| *time)
            .collect::<Vec<_>>();
        assert!(api_times.len() > 10);
        for pair in api_times.windows(2) {
            assert!(pair[1] - pair[0] >= Duration::from_millis(55));
        }
        drop((requests, times));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_failed_gtfs_download_is_retried() {
        let (client, sources, state) = mock_client(false, false).await;
        state.script_failure("/gtfs", 503, None);
        let root = temporary_root("gtfs-retry");

        let snapshot = fetch_snapshot_at(&root, "20260928T041543Z", None, &client, &sources)
            .await
            .unwrap();

        assert_eq!(state.request_count("/gtfs"), 2);
        assert_eq!(
            fs::read(snapshot.path.join("gtfs.zip")).unwrap(),
            mock_gtfs_zip()
        );
        let names = fs::read_dir(&snapshot.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(!names.iter().any(|name| name.ends_with(".part")));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn an_optional_catalog_that_keeps_failing_reuses_the_previous_snapshot_copy() {
        let (client, sources, state) = mock_client(false, false).await;
        let root = temporary_root("fallback");
        let previous = fetch_with_local_gtfs(&root, "20260928T041550Z", &client, &sources)
            .await
            .unwrap();
        promote_latest(&root, "20260928T041550Z");
        state
            .fail_new_taipei
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let snapshot = fetch_with_local_gtfs(&root, "20260928T041551Z", &client, &sources)
            .await
            .unwrap();

        assert_eq!(
            fs::read(snapshot.path.join("bike-stations-new-taipei.json")).unwrap(),
            fs::read(previous.path.join("bike-stations-new-taipei.json")).unwrap()
        );
        assert_eq!(
            snapshot.manifest.reused_sources.keys().collect::<Vec<_>>(),
            ["bike-stations-new-taipei"]
        );
        assert_eq!(
            snapshot.manifest.reused_sources["bike-stations-new-taipei"],
            previous.manifest.fetched_at
        );
        assert_eq!(snapshot.bike_station_counts["NewTaipei"], 2);
        let on_disk = serde_json::from_slice::<FetchManifest>(
            &fs::read(snapshot.path.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert!(
            on_disk
                .reused_sources
                .contains_key("bike-stations-new-taipei")
        );
        read_snapshot(&snapshot.path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn gtfs_only_downloads_gtfs_and_reuses_every_previous_catalog() {
        let (client, mut sources, state) = mock_client(false, false).await;
        let root = temporary_root("gtfs-only");
        let previous = fetch_with_local_gtfs(&root, "20260928T041560Z", &client, &sources)
            .await
            .unwrap();
        promote_latest(&root, "20260928T041560Z");
        let before = state.requests.lock().unwrap().len();
        sources.policy.gtfs_only = true;

        let snapshot = fetch_snapshot_at(
            &root.join("fetch"),
            "20260928T041561Z",
            None,
            &client,
            &sources,
        )
        .await
        .unwrap();

        let later = state.requests.lock().unwrap()[before..].to_vec();
        assert!(
            later
                .iter()
                .all(|path| path.starts_with("/token") || path == "/gtfs"),
            "{later:?}"
        );
        assert!(later.iter().any(|path| path == "/gtfs"));
        let reused = snapshot
            .manifest
            .reused_sources
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let expected = previous
            .manifest
            .sources
            .keys()
            .filter(|name| name.as_str() != GTFS_SOURCE_NAME)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(reused, expected);
        assert_eq!(snapshot.bike_station_counts["Taipei"], 2);
        read_snapshot(&snapshot.path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn gtfs_only_without_a_previous_snapshot_fails_without_publishing() {
        let (client, mut sources, state) = mock_client(false, false).await;
        sources.policy.gtfs_only = true;
        let root = temporary_root("gtfs-only-empty");

        let error = fetch_snapshot_at(&root, "20260928T041570Z", None, &client, &sources)
            .await
            .unwrap_err();

        assert!(
            matches!(error, FetchError::MissingPreviousSource(_)),
            "{error}"
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(state.request_count("/basic/"), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
