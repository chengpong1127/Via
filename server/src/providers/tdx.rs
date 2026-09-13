use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;

use crate::{
    live_status::{FreshnessPolicy, LiveStatusTimes},
    normalization::{
        ActionState, ArrivalEstimate, ArrivalState, BikeAvailability, BikeShareStationId,
        BoardingPointId, LocalizedSourceText, ServiceCondition, ServiceConditionState,
        TransitServiceId,
    },
    realtime::{
        ArrivalQuery, NormalizedRealtimeValue, RealtimeError, RealtimeProvider, RealtimeValue,
    },
};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROVIDER_CLOCK_SKEW: TimeDuration = TimeDuration::minutes(5);
const DEFAULT_ARRIVAL_PATH: &str = "v2/Bus/EstimatedTimeOfArrival/City";
const DEFAULT_BIKE_PATH: &str = "v2/Bike/Availability/Station";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TdxRealtimeConfig {
    pub base_url: Url,
    pub authority_namespace: String,
}

pub use crate::domain::ids::IdentityRepository as IdentityResolver;

struct RejectUnknownIdentity;

#[async_trait]
impl IdentityResolver for RejectUnknownIdentity {
    async fn resolve(&self, _: &str, _: &str, _: &str) -> Option<String> {
        None
    }

    async fn upstream_key(&self, _: &str, _: &str, _: &str) -> Option<String> {
        None
    }
}

pub struct TdxRealtimeProvider {
    base_url: Url,
    authority_namespace: String,
    arrival_path: String,
    bike_path: String,
    condition_path: Option<String>,
    client: Client,
    policy: FreshnessPolicy,
    identities: Arc<dyn IdentityResolver>,
}

#[derive(Debug, thiserror::Error)]
pub enum TdxConfigError {
    #[error("TDX realtime configuration is invalid: {0}")]
    InvalidConfiguration(&'static str),
    #[error("TDX bearer token must not be empty")]
    EmptyBearerToken,
    #[error("TDX bearer token contains invalid header characters")]
    InvalidBearerToken(#[from] header::InvalidHeaderValue),
    #[error("TDX HTTP client configuration failed")]
    Client(#[from] reqwest::Error),
}

impl TdxRealtimeProvider {
    pub fn new(config: TdxRealtimeConfig, bearer_token: &str) -> Result<Self, TdxConfigError> {
        if config.authority_namespace.trim().is_empty() {
            return Err(TdxConfigError::InvalidConfiguration(
                "authority namespace must not be empty",
            ));
        }
        if config.base_url.host_str().is_none() {
            return Err(TdxConfigError::InvalidConfiguration(
                "TDX base URL must include a host",
            ));
        }
        if bearer_token.is_empty() {
            return Err(TdxConfigError::EmptyBearerToken);
        }
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {bearer_token}"))?,
        );
        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(8))
            .user_agent("via-server/1.0")
            .build()?;
        Ok(Self {
            base_url: config.base_url,
            authority_namespace: config.authority_namespace,
            arrival_path: DEFAULT_ARRIVAL_PATH.into(),
            bike_path: DEFAULT_BIKE_PATH.into(),
            condition_path: None,
            client,
            policy: FreshnessPolicy::tdx_one_minute("1"),
            identities: Arc::new(RejectUnknownIdentity),
        })
    }

    pub fn with_identity_resolver(mut self, identities: Arc<dyn IdentityResolver>) -> Self {
        self.identities = identities;
        self
    }

    pub fn with_endpoint_paths(
        mut self,
        arrival_path: impl Into<String>,
        bike_path: impl Into<String>,
    ) -> Self {
        self.arrival_path = arrival_path.into();
        self.bike_path = bike_path.into();
        self
    }

    pub fn with_service_condition_path(mut self, path: impl Into<String>) -> Self {
        self.condition_path = Some(path.into());
        self
    }

    fn endpoint(&self, path: &str) -> Result<Url, RealtimeError> {
        self.base_url
            .join(path)
            .map_err(|_| RealtimeError::InvalidConfiguration)
    }

    async fn get_json(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<serde_json::Value, RealtimeError> {
        let mut url = self.endpoint(path)?;
        url.query_pairs_mut().extend_pairs(
            query
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        let response = self.client.get(url).send().await.map_err(|error| {
            if error.is_timeout() {
                RealtimeError::TimedOut
            } else {
                RealtimeError::TemporarilyUnavailable
            }
        })?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(RealtimeError::RateLimited);
        }
        if !response.status().is_success() {
            return Err(RealtimeError::TemporarilyUnavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(RealtimeError::RejectedPayload);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| RealtimeError::TemporarilyUnavailable)?;
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(RealtimeError::RejectedPayload);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| RealtimeError::RejectedPayload)
    }

    async fn upstream_ids(
        &self,
        entity_kind: &str,
        via_ids: impl IntoIterator<Item = String>,
    ) -> Result<Vec<String>, RealtimeError> {
        let mut upstream_ids = Vec::new();
        for via_id in via_ids {
            let Some(upstream_id) = self
                .identities
                .upstream_key(entity_kind, &self.authority_namespace, &via_id)
                .await
            else {
                return Err(RealtimeError::UnknownIdentity);
            };
            upstream_ids.push(upstream_id);
        }
        upstream_ids.sort();
        upstream_ids.dedup();
        Ok(upstream_ids)
    }

    async fn normalize_payload(
        &self,
        payload: &serde_json::Value,
        family: TdxFamily,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        let records = records(payload);
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let received_at = OffsetDateTime::now_utc();
        let normalized_at = OffsetDateTime::now_utc();
        let mut normalized = Vec::new();
        let mut saw_published_record = false;
        for record in records {
            let Some(published_at) =
                time_field(record, "UpdateTime").or_else(|| time_field(record, "update_time"))
            else {
                continue;
            };
            saw_published_record = true;
            if published_at > normalized_at + MAX_PROVIDER_CLOCK_SKEW {
                return Err(RealtimeError::RejectedPayload);
            }
            let freshness = self.policy.evaluate(
                LiveStatusTimes {
                    measured_at: None,
                    published_at: Some(published_at),
                    received_at,
                    normalized_at,
                },
                normalized_at,
                None,
            );
            let value = match family {
                TdxFamily::BusArrival => self.normalize_bus_arrival(record, published_at).await,
                TdxFamily::BikeAvailability => {
                    self.normalize_bike_station(record, published_at).await
                }
                TdxFamily::ServiceCondition => {
                    self.normalize_service_condition(record, published_at).await
                }
            };
            let Some((id, value)) = value else { continue };
            let source_change_at = match &value {
                RealtimeValue::BikeAvailability(availability) => availability.source_change_at,
                _ => None,
            };
            normalized.push(NormalizedRealtimeValue {
                id,
                received_at,
                normalized_at,
                provider_published_at: Some(published_at),
                source_change_at,
                freshness: freshness.clone(),
                value,
            });
        }
        if normalized.is_empty() {
            return Err(if saw_published_record {
                RealtimeError::UnknownIdentity
            } else {
                RealtimeError::RejectedPayload
            });
        }
        Ok(normalized)
    }

    async fn normalize_bus_arrival(
        &self,
        record: &serde_json::Value,
        published_at: OffsetDateTime,
    ) -> Option<(String, RealtimeValue)> {
        let stop_uid = string_field(record, "StopUID")?;
        let service_uid = string_field(record, "SubRouteUID")?;
        let boarding_point_id = self
            .identities
            .resolve("boarding_point", &self.authority_namespace, stop_uid)
            .await?;
        let transit_service_id = self
            .identities
            .resolve("transit_service", &self.authority_namespace, service_uid)
            .await?;
        let status = record
            .get("StopStatus")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(u8::MAX);
        let estimate = record
            .get("EstimateTime")
            .and_then(serde_json::Value::as_i64);
        let value = normalize_n1(
            TransitServiceId::new(transit_service_id),
            BoardingPointId::new(boarding_point_id),
            status,
            estimate,
            published_at,
        );
        Some((
            observation_id(
                &self.authority_namespace,
                stop_uid,
                service_uid,
                published_at,
            ),
            RealtimeValue::ArrivalEstimate(value),
        ))
    }

    async fn normalize_bike_station(
        &self,
        record: &serde_json::Value,
        published_at: OffsetDateTime,
    ) -> Option<(String, RealtimeValue)> {
        let station_uid = string_field(record, "StationUID")?;
        let station_id = self
            .identities
            .resolve("bike_share_station", &self.authority_namespace, station_uid)
            .await?;
        let service_status = record
            .get("ServiceStatus")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(u8::MAX);
        let source_change_at = time_field(record, "SrcUpdateTime");
        let received_at = OffsetDateTime::now_utc();
        let value = normalize_bike_availability(
            BikeShareStationId::new(station_id),
            service_status,
            u32_field(record, "AvailableRentBikes"),
            u32_field(record, "AvailableReturnBikes"),
            source_change_at,
            Some(published_at),
            received_at,
        );
        Some((
            observation_id(&self.authority_namespace, station_uid, "bike", published_at),
            RealtimeValue::BikeAvailability(value),
        ))
    }

    async fn normalize_service_condition(
        &self,
        record: &serde_json::Value,
        published_at: OffsetDateTime,
    ) -> Option<(String, RealtimeValue)> {
        let service_uid =
            string_field(record, "SubRouteUID").or_else(|| string_field(record, "ServiceUID"))?;
        let service_id = self
            .identities
            .resolve("transit_service", &self.authority_namespace, service_uid)
            .await?;
        let status = string_field(record, "Status")
            .or_else(|| string_field(record, "Condition"))
            .unwrap_or("disrupted")
            .to_ascii_lowercase();
        let state = match status.as_str() {
            "suspended" | "suspend" => ServiceConditionState::Suspended,
            "closed" | "close" => ServiceConditionState::Closed,
            _ => ServiceConditionState::Disrupted,
        };
        let source_text_variants = string_field(record, "Message")
            .or_else(|| string_field(record, "Description"))
            .map(|text| {
                vec![LocalizedSourceText {
                    locale: string_field(record, "Language").unwrap_or("und").into(),
                    text: text.into(),
                }]
            })
            .unwrap_or_default();
        let condition = ServiceCondition {
            state,
            scope_via_ids: vec![service_id],
            source_text_variants,
            effective_from: time_field(record, "EffectiveFrom"),
            effective_until: time_field(record, "EffectiveUntil"),
        };
        Some((
            observation_id(
                &self.authority_namespace,
                service_uid,
                "condition",
                published_at,
            ),
            RealtimeValue::ServiceCondition(condition),
        ))
    }
}

#[derive(Clone, Copy)]
enum TdxFamily {
    BusArrival,
    BikeAvailability,
    ServiceCondition,
}

#[async_trait]
impl RealtimeProvider for TdxRealtimeProvider {
    async fn arrival_estimates(
        &self,
        query: ArrivalQuery,
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        if query.transit_service_ids.is_empty() || query.boarding_point_ids.is_empty() {
            return Ok(Vec::new());
        }
        let service_via_ids = query
            .transit_service_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        let boarding_via_ids = query
            .boarding_point_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        let service_ids = self
            .upstream_ids("transit_service", service_via_ids)
            .await?;
        let boarding_point_ids = self
            .upstream_ids("boarding_point", boarding_via_ids)
            .await?;
        let query_params = vec![(
            "$filter".into(),
            format!(
                "({}) and ({})",
                odata_in_filter("SubRouteUID", &service_ids),
                odata_in_filter("StopUID", &boarding_point_ids)
            ),
        )];
        let payload = self.get_json(&self.arrival_path, &query_params).await?;
        let values = self
            .normalize_payload(&payload, TdxFamily::BusArrival)
            .await?;
        let requested_services = query
            .transit_service_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let requested_boarding_points = query
            .boarding_point_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        Ok(values
            .into_iter()
            .filter(|value| {
                matches!(
                    &value.value,
                    RealtimeValue::ArrivalEstimate(arrival)
                        if requested_services.contains(&arrival.transit_service_id)
                            && requested_boarding_points.contains(&arrival.boarding_point_id)
                )
            })
            .collect())
    }

    async fn bike_availability(
        &self,
        station_ids: &[BikeShareStationId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        if station_ids.is_empty() {
            return Ok(Vec::new());
        }
        let station_ids = station_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        let upstream_ids = self.upstream_ids("bike_share_station", station_ids).await?;
        let query_params: Vec<(String, String)> = vec![(
            "$filter".into(),
            odata_in_filter("StationUID", &upstream_ids),
        )];
        let payload = self.get_json(&self.bike_path, &query_params).await?;
        self.normalize_payload(&payload, TdxFamily::BikeAvailability)
            .await
    }

    async fn service_conditions(
        &self,
        service_ids: &[TransitServiceId],
    ) -> Result<Vec<NormalizedRealtimeValue>, RealtimeError> {
        if service_ids.is_empty() {
            return Ok(Vec::new());
        }
        let Some(path) = &self.condition_path else {
            return Ok(Vec::new());
        };
        let service_via_ids = service_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        let upstream_ids = self
            .upstream_ids("transit_service", service_via_ids)
            .await?;
        let query_params: Vec<(String, String)> = vec![(
            "$filter".into(),
            odata_in_filter("SubRouteUID", &upstream_ids),
        )];
        let payload = self.get_json(path, &query_params).await?;
        let values = self
            .normalize_payload(&payload, TdxFamily::ServiceCondition)
            .await?;
        Ok(values
            .into_iter()
            .filter(|value| {
                matches!(
                    &value.value,
                    RealtimeValue::ServiceCondition(condition)
                        if condition.scope_via_ids.iter().any(|service_id| {
                            service_ids.iter().any(|requested| requested.as_str() == service_id)
                        })
                )
            })
            .collect())
    }
}

fn odata_in_filter(field: &str, values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!("{field} in ({values})")
}

fn records(payload: &serde_json::Value) -> &[serde_json::Value] {
    payload
        .as_array()
        .or_else(|| payload.get("value").and_then(serde_json::Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn string_field<'a>(record: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    record.get(field).and_then(serde_json::Value::as_str)
}

fn time_field(record: &serde_json::Value, field: &str) -> Option<OffsetDateTime> {
    string_field(record, field).and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
}

fn u32_field(record: &serde_json::Value, field: &str) -> Option<u32> {
    record
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn observation_id(
    capability: &str,
    primary_key: &str,
    secondary_key: &str,
    published_at: OffsetDateTime,
) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"via.realtime.v1\0");
    digest.update(capability.as_bytes());
    digest.update(primary_key.as_bytes());
    digest.update(secondary_key.as_bytes());
    digest.update(published_at.unix_timestamp_nanos().to_be_bytes());
    format!("live_{:x}", digest.finalize())
}

pub fn normalize_n1(
    transit_service_id: TransitServiceId,
    boarding_point_id: BoardingPointId,
    stop_status: u8,
    estimate_seconds: Option<i64>,
    transmitted_at: OffsetDateTime,
) -> ArrivalEstimate {
    let (state, predicted_arrival_at) = match stop_status {
        0 => match estimate_seconds {
            Some(0) => (ArrivalState::AtBoardingPoint, Some(transmitted_at)),
            Some(seconds) if seconds > 0 => (
                ArrivalState::Predicted,
                Some(transmitted_at + time::Duration::seconds(seconds)),
            ),
            _ => (ArrivalState::Unknown, None),
        },
        1 => (ArrivalState::NotDeparted, None),
        2 => (ArrivalState::Skipped, None),
        3 => (ArrivalState::LastServicePassed, None),
        4 => (ArrivalState::NotOperating, None),
        _ => (ArrivalState::Unknown, None),
    };
    ArrivalEstimate {
        transit_service_id,
        boarding_point_id,
        predicted_arrival_at,
        state,
        transmitted_at,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn normalize_bike_availability(
    station_id: BikeShareStationId,
    service_status: u8,
    available_bikes: Option<u32>,
    available_return_slots: Option<u32>,
    source_change_at: Option<OffsetDateTime>,
    provider_published_at: Option<OffsetDateTime>,
    received_at: OffsetDateTime,
) -> BikeAvailability {
    let state = match service_status {
        1 => ActionState::Enabled,
        0 | 2 => ActionState::Disabled,
        _ => ActionState::Unknown,
    };
    BikeAvailability {
        station_id,
        rental_state: state,
        return_state: state,
        available_bikes,
        available_return_slots,
        source_change_at,
        provider_published_at,
        received_at,
    }
}
