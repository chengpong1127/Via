use std::{collections::BTreeMap, sync::Arc};

use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, UtcOffset};

use crate::{
    api::{PlanningRequest, RequestCoordinate, SelectedMode},
    snapshot::{NetworkSnapshot, SnapshotError, SnapshotStore},
    street::{
        Attribution, Coordinate, DetailedPath, DetailedPathQuery, Maneuver, ManeuverKind,
        ProviderError, RoutingConstraints, StreetRoutingSession, StreetTravelMode,
    },
};

#[derive(Clone)]
pub struct JourneyPlanningService {
    street_routing: StreetRoutingSession,
    snapshots: Arc<dyn SnapshotStore>,
}

impl JourneyPlanningService {
    pub fn new(street_routing: StreetRoutingSession, snapshots: Arc<dyn SnapshotStore>) -> Self {
        Self {
            street_routing,
            snapshots,
        }
    }

    pub async fn plan(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
        let snapshot = self.snapshots.pin_current().await?;
        let origin = to_street_coordinate(request.origin.coordinate)?;
        let destination = to_street_coordinate(request.destination.coordinate)?;
        let path = match self
            .street_routing
            .detailed_path(DetailedPathQuery {
                travel_mode: StreetTravelMode::Walking,
                origin,
                destination,
                constraints: RoutingConstraints::default(),
            })
            .await
        {
            Ok(path) => Some(path),
            Err(ProviderError::UnreachableEndpoints) => None,
            Err(error) => return Err(error.into()),
        };

        let generated_at = taipei_now();
        let request_id = next_request_id();
        let metadata = ResultMetadata::new(request_id, generated_at, snapshot, &request);
        let Some(path) = path else {
            return Ok(PlanningOutcome::Empty(EmptyResult {
                metadata,
                empty: SemanticObject {
                    code: "no_feasible_journey_option",
                    required: true,
                    parameters: BTreeMap::from([(
                        "selected_modes",
                        serde_json::to_value(&request.selected_modes).unwrap_or_default(),
                    )]),
                },
            }));
        };

        let (departure_time, arrival_time) = request
            .departure_setting
            .resolve(metadata.effective_search_time, path.duration.0);
        let option = walking_option(&request, path, departure_time, arrival_time);
        Ok(PlanningOutcome::Options(OptionsResult {
            metadata,
            journey_options: vec![option],
        }))
    }
}

#[derive(Debug)]
pub enum PlanningError {
    SnapshotUnavailable,
    TemporarilyUnavailable,
    Internal,
}

impl From<SnapshotError> for PlanningError {
    fn from(_: SnapshotError) -> Self {
        Self::SnapshotUnavailable
    }
}

impl From<ProviderError> for PlanningError {
    fn from(error: ProviderError) -> Self {
        match error {
            ProviderError::Timeout
            | ProviderError::RateLimited
            | ProviderError::TemporarilyUnavailable => Self::TemporarilyUnavailable,
            ProviderError::UnreachableEndpoints => unreachable_is_handled_before_conversion(),
            ProviderError::InvalidRequest { .. }
            | ProviderError::MalformedResponse
            | ProviderError::UnsupportedCapability
            | ProviderError::InconsistentGraphRevision { .. }
            | ProviderError::ResponseTooLarge { .. } => Self::Internal,
        }
    }
}

fn unreachable_is_handled_before_conversion() -> ! {
    unreachable!("unreachable paths are converted to successful empty results")
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum PlanningOutcome {
    Options(OptionsResult),
    Empty(EmptyResult),
}

#[derive(Clone, Debug, Serialize)]
pub struct OptionsResult {
    #[serde(flatten)]
    pub metadata: ResultMetadata,
    pub journey_options: Vec<JourneyOption>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EmptyResult {
    #[serde(flatten)]
    pub metadata: ResultMetadata,
    pub empty: SemanticObject,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResultMetadata {
    pub api_version: &'static str,
    pub semantic_catalog_version: &'static str,
    pub request_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub effective_search_time: OffsetDateTime,
    pub time_zone: &'static str,
    pub network_snapshot: NetworkSnapshot,
    pub policy_versions: PolicyVersions,
    pub warnings: Vec<Warning>,
}

impl ResultMetadata {
    fn new(
        request_id: String,
        generated_at: OffsetDateTime,
        network_snapshot: NetworkSnapshot,
        request: &PlanningRequest,
    ) -> Self {
        let effective_search_time = request
            .departure_setting
            .effective_search_time(generated_at);
        Self {
            api_version: crate::api::API_VERSION,
            semantic_catalog_version: crate::api::SEMANTIC_CATALOG_VERSION,
            request_id,
            generated_at,
            effective_search_time,
            time_zone: "Asia/Taipei",
            policy_versions: PolicyVersions {
                freshness: network_snapshot.freshness_policy_version.clone(),
                ranking: "1".into(),
                transfer_safety: "1".into(),
            },
            network_snapshot,
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PolicyVersions {
    pub freshness: String,
    pub ranking: String,
    pub transfer_safety: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct JourneyOption {
    pub id: String,
    pub matching_key: String,
    pub class: &'static str,
    pub label: Option<&'static str>,
    pub ranking_reasons: Vec<&'static str>,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    pub duration_seconds: u32,
    pub walking_seconds: u32,
    pub transfer_count: u32,
    pub travel_modes: Vec<&'static str>,
    pub live_status_summary: LiveStatusSummary,
    pub overview_geometry: Geometry,
    pub legs: Vec<Leg>,
    pub transfers: Vec<Transfer>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Geometry {
    pub encoding: &'static str,
    pub points: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Leg {
    pub id: String,
    pub mode: &'static str,
    pub from: Place,
    pub to: Place,
    #[serde(with = "time::serde::rfc3339")]
    pub departure_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub arrival_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_departure_time: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_arrival_time: Option<OffsetDateTime>,
    pub duration_seconds: u32,
    pub distance_metres: u32,
    pub geometry: Geometry,
    pub guidance: Vec<Guidance>,
    pub details: serde_json::Value,
    pub live_status: Vec<serde_json::Value>,
    pub warnings: Vec<Warning>,
    pub attribution: Vec<Attribution>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Place {
    pub display_name: String,
    pub coordinate: RequestCoordinate,
    pub kind: &'static str,
    pub via_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Guidance {
    pub code: &'static str,
    pub required: bool,
    pub parameters: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Transfer {
    pub from_leg_id: String,
    pub to_leg_id: String,
    pub connector_leg_ids: Vec<String>,
    pub duration_seconds: u32,
    pub available_seconds: u32,
    pub class: &'static str,
    pub reasons: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveStatusSummary {
    pub state: &'static str,
    pub reasons: Vec<&'static str>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub required: bool,
    pub severity: &'static str,
    pub parameters: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticObject {
    pub code: &'static str,
    pub required: bool,
    pub parameters: BTreeMap<&'static str, serde_json::Value>,
}

fn walking_option(
    request: &PlanningRequest,
    path: DetailedPath,
    departure_time: OffsetDateTime,
    arrival_time: OffsetDateTime,
) -> JourneyOption {
    let encoded = encode_polyline6(&path.geometry);
    let geometry = Geometry {
        encoding: "polyline6",
        points: encoded.clone(),
    };
    let leg = Leg {
        id: "leg_1".into(),
        mode: "walking",
        from: Place {
            display_name: request.origin.display_text.clone(),
            coordinate: request.origin.coordinate,
            kind: "selected_place",
            via_id: None,
        },
        to: Place {
            display_name: request.destination.display_text.clone(),
            coordinate: request.destination.coordinate,
            kind: "selected_place",
            via_id: None,
        },
        departure_time,
        arrival_time,
        scheduled_departure_time: None,
        scheduled_arrival_time: None,
        duration_seconds: path.duration.0,
        distance_metres: path.distance.0,
        geometry: Geometry {
            encoding: "polyline6",
            points: encoded,
        },
        guidance: guidance(&path.maneuvers),
        details: serde_json::json!({
            "type": "walking",
            "start_fact": null,
            "end_fact": null
        }),
        live_status: Vec::new(),
        warnings: Vec::new(),
        attribution: path.attribution,
    };
    JourneyOption {
        id: "option_1".into(),
        matching_key: matching_key(request),
        class: "reliable",
        label: Some("best_now"),
        ranking_reasons: vec!["earliest_reliable_arrival"],
        departure_time,
        arrival_time,
        duration_seconds: path.duration.0,
        walking_seconds: path.duration.0,
        transfer_count: 0,
        travel_modes: vec!["walking"],
        live_status_summary: LiveStatusSummary {
            state: "not_applicable",
            reasons: Vec::new(),
            observed_at: None,
        },
        overview_geometry: geometry,
        legs: vec![leg],
        transfers: Vec::new(),
        warnings: Vec::new(),
    }
}

fn guidance(maneuvers: &[Maneuver]) -> Vec<Guidance> {
    maneuvers
        .iter()
        .map(|maneuver| Guidance {
            code: match maneuver.kind {
                ManeuverKind::Depart => "depart",
                ManeuverKind::Continue => "continue",
                ManeuverKind::TurnLeft => "turn_left",
                ManeuverKind::TurnRight => "turn_right",
                ManeuverKind::UTurn => "u_turn",
                ManeuverKind::EnterRoundabout => "enter_roundabout",
                ManeuverKind::ExitRoundabout => "exit_roundabout",
                ManeuverKind::Arrive => "arrive",
            },
            required: true,
            parameters: BTreeMap::new(),
        })
        .collect()
}

fn matching_key(request: &PlanningRequest) -> String {
    let mut digest = Sha256::new();
    digest.update(b"via.walking.v1\0");
    digest.update(request.origin.coordinate.latitude.to_be_bytes());
    digest.update(request.origin.coordinate.longitude.to_be_bytes());
    digest.update(request.destination.coordinate.latitude.to_be_bytes());
    digest.update(request.destination.coordinate.longitude.to_be_bytes());
    format!("match_{:x}", digest.finalize())
}

fn encode_polyline6(coordinates: &[Coordinate]) -> String {
    let mut output = String::new();
    let mut previous_latitude = 0_i64;
    let mut previous_longitude = 0_i64;
    for coordinate in coordinates {
        let latitude = (coordinate.latitude() * 1_000_000.0).round() as i64;
        let longitude = (coordinate.longitude() * 1_000_000.0).round() as i64;
        encode_polyline_value(latitude - previous_latitude, &mut output);
        encode_polyline_value(longitude - previous_longitude, &mut output);
        previous_latitude = latitude;
        previous_longitude = longitude;
    }
    output
}

fn encode_polyline_value(value: i64, output: &mut String) {
    let mut encoded = if value < 0 {
        (!(value << 1)) as u64
    } else {
        (value << 1) as u64
    };
    while encoded >= 0x20 {
        output.push(char::from_u32(((0x20 | (encoded & 0x1f)) + 63) as u32).unwrap());
        encoded >>= 5;
    }
    output.push(char::from_u32((encoded + 63) as u32).unwrap());
}

fn to_street_coordinate(coordinate: RequestCoordinate) -> Result<Coordinate, PlanningError> {
    Coordinate::try_new(coordinate.latitude, coordinate.longitude).map_err(PlanningError::from)
}

fn taipei_now() -> OffsetDateTime {
    OffsetDateTime::now_utc().to_offset(UtcOffset::from_hms(8, 0, 0).unwrap())
}

fn next_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
    format!(
        "req_{:016x}",
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn selected_modes_as_strings(modes: &[SelectedMode]) -> Vec<&'static str> {
    modes
        .iter()
        .map(|mode| match mode {
            SelectedMode::Bus => "bus",
            SelectedMode::Metro => "metro",
            SelectedMode::YouBike => "youbike",
            SelectedMode::Unknown => "unknown",
        })
        .collect()
}
