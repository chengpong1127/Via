use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;

use crate::{
    live_status::{FreshnessPolicy, FreshnessReason, LiveStatusTimes},
    normalization::{
        ActionState, ArrivalEstimate, ArrivalState, BikeAvailability, BoardingPointId,
        TransitServiceId, YouBikeStationId,
    },
    refresh::{FeedRefreshBatch, FeedSource},
    snapshot::{LiveStatusValue, NormalizedLiveStatus},
};

const MAX_FEED_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROVIDER_CLOCK_SKEW: TimeDuration = TimeDuration::minutes(5);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TdxFeedConfig {
    pub capability: String,
    pub url: Url,
    pub family: TdxFeedFamily,
    pub authority_namespace: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TdxFeedFamily {
    BusArrival,
    BikeAvailability,
    ServiceCondition,
}

#[async_trait]
pub trait IdentityResolver: Send + Sync {
    async fn resolve(
        &self,
        entity_kind: &str,
        authority_namespace: &str,
        upstream_key: &str,
    ) -> Option<String>;
}

struct RejectUnknownIdentity;

#[async_trait]
impl IdentityResolver for RejectUnknownIdentity {
    async fn resolve(&self, _: &str, _: &str, _: &str) -> Option<String> {
        None
    }
}

pub struct TdxFeedSource {
    capability: String,
    url: Url,
    client: Client,
    policy: FreshnessPolicy,
    family: TdxFeedFamily,
    authority_namespace: String,
    identities: Arc<dyn IdentityResolver>,
}

#[derive(Debug, thiserror::Error)]
pub enum TdxConfigError {
    #[error("TDX feed configuration is invalid: {0}")]
    InvalidConfiguration(&'static str),
    #[error("TDX bearer token must not be empty")]
    EmptyBearerToken,
    #[error("TDX bearer token contains invalid header characters")]
    InvalidBearerToken(#[from] header::InvalidHeaderValue),
    #[error("TDX HTTP client configuration failed")]
    Client(#[from] reqwest::Error),
}

impl TdxFeedSource {
    pub fn new(config: TdxFeedConfig, bearer_token: &str) -> Result<Self, TdxConfigError> {
        if config.capability.trim().is_empty() {
            return Err(TdxConfigError::InvalidConfiguration(
                "capability must not be empty",
            ));
        }
        if config.authority_namespace.trim().is_empty() {
            return Err(TdxConfigError::InvalidConfiguration(
                "authority namespace must not be empty",
            ));
        }
        if config.url.host_str().is_none() {
            return Err(TdxConfigError::InvalidConfiguration(
                "feed URL must include a host",
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
            capability: config.capability,
            url: config.url,
            family: config.family,
            authority_namespace: config.authority_namespace,
            client,
            policy: FreshnessPolicy::tdx_one_minute("1"),
            identities: Arc::new(RejectUnknownIdentity),
        })
    }

    pub fn with_identity_resolver(mut self, identities: Arc<dyn IdentityResolver>) -> Self {
        self.identities = identities;
        self
    }
}

#[async_trait]
impl FeedSource for TdxFeedSource {
    fn capability(&self) -> &str {
        &self.capability
    }

    fn freshness_policy(&self, version: &str) -> FreshnessPolicy {
        let mut policy = self.policy.clone();
        policy.version = version.into();
        policy
    }

    async fn refresh(
        &self,
        _refresh_started_at: OffsetDateTime,
    ) -> Result<FeedRefreshBatch, FreshnessReason> {
        let response = self
            .client
            .get(self.url.clone())
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    FreshnessReason::TimedOut
                } else {
                    FreshnessReason::SourceError
                }
            })?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(FreshnessReason::RateLimited);
        }
        if !response.status().is_success() {
            return Err(FreshnessReason::SourceError);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_FEED_BYTES as u64)
        {
            return Err(FreshnessReason::RejectedPayload);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| FreshnessReason::SourceError)?;
            if body.len().saturating_add(chunk.len()) > MAX_FEED_BYTES {
                return Err(FreshnessReason::RejectedPayload);
            }
            body.extend_from_slice(&chunk);
        }
        let received_at = OffsetDateTime::now_utc();
        let payload: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| FreshnessReason::RejectedPayload)?;
        if records(&payload).is_empty() {
            tracing::warn!(capability = %self.capability, "TDX payload contained no records");
            return Err(FreshnessReason::RejectedPayload);
        }
        let normalized_at = OffsetDateTime::now_utc();
        let published_at = newest_update_time(&payload).ok_or(FreshnessReason::RejectedPayload)?;
        if published_at > normalized_at + MAX_PROVIDER_CLOCK_SKEW {
            return Err(FreshnessReason::RejectedPayload);
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
        let live_status = match self.family {
            TdxFeedFamily::BusArrival => {
                self.normalize_bus_arrivals(&payload, received_at, normalized_at)
                    .await
            }
            TdxFeedFamily::BikeAvailability => {
                self.normalize_bike_stations(&payload, received_at, normalized_at)
                    .await
            }
            TdxFeedFamily::ServiceCondition => {
                return Err(FreshnessReason::RejectedPayload);
            }
        };
        if live_status.is_empty() {
            tracing::warn!(
                capability = %self.capability,
                "TDX payload produced no normalized observations"
            );
            return Err(FreshnessReason::RejectedPayload);
        }
        Ok(FeedRefreshBatch {
            freshness,
            live_status,
        })
    }
}

impl TdxFeedSource {
    async fn normalize_bus_arrivals(
        &self,
        payload: &serde_json::Value,
        received_at: OffsetDateTime,
        normalized_at: OffsetDateTime,
    ) -> Vec<NormalizedLiveStatus> {
        let mut normalized = Vec::new();
        for record in records(payload) {
            let Some(stop_uid) = string_field(record, "StopUID") else {
                tracing::warn!(capability = %self.capability, "TDX bus record missing StopUID");
                continue;
            };
            let Some(service_uid) = string_field(record, "SubRouteUID") else {
                tracing::warn!(capability = %self.capability, "TDX bus record missing SubRouteUID");
                continue;
            };
            let Some(published_at) = time_field(record, "UpdateTime") else {
                tracing::warn!(capability = %self.capability, "TDX bus record missing UpdateTime");
                continue;
            };
            let Some(boarding_point_id) = self
                .identities
                .resolve("boarding_point", &self.authority_namespace, stop_uid)
                .await
            else {
                tracing::warn!(
                    capability = %self.capability,
                    entity_kind = "boarding_point",
                    "TDX record dropped because its identity alias is unknown"
                );
                continue;
            };
            let Some(transit_service_id) = self
                .identities
                .resolve("transit_service", &self.authority_namespace, service_uid)
                .await
            else {
                tracing::warn!(
                    capability = %self.capability,
                    entity_kind = "transit_service",
                    "TDX record dropped because its identity alias is unknown"
                );
                continue;
            };
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
            normalized.push(NormalizedLiveStatus {
                id: observation_id(&self.capability, stop_uid, service_uid, published_at),
                capability: self.capability.clone(),
                received_at,
                normalized_at,
                freshness: self.policy.evaluate(
                    LiveStatusTimes {
                        measured_at: None,
                        published_at: Some(published_at),
                        received_at,
                        normalized_at,
                    },
                    normalized_at,
                    None,
                ),
                value: LiveStatusValue::ArrivalEstimate(value),
            });
        }
        normalized
    }

    async fn normalize_bike_stations(
        &self,
        payload: &serde_json::Value,
        received_at: OffsetDateTime,
        normalized_at: OffsetDateTime,
    ) -> Vec<NormalizedLiveStatus> {
        let mut normalized = Vec::new();
        for record in records(payload) {
            let Some(station_uid) = string_field(record, "StationUID") else {
                tracing::warn!(capability = %self.capability, "TDX bike record missing StationUID");
                continue;
            };
            let Some(published_at) = time_field(record, "UpdateTime") else {
                tracing::warn!(capability = %self.capability, "TDX bike record missing UpdateTime");
                continue;
            };
            let Some(station_id) = self
                .identities
                .resolve("youbike_station", &self.authority_namespace, station_uid)
                .await
            else {
                tracing::warn!(
                    capability = %self.capability,
                    entity_kind = "youbike_station",
                    "TDX record dropped because its identity alias is unknown"
                );
                continue;
            };
            let service_status = record
                .get("ServiceStatus")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u8::try_from(value).ok())
                .unwrap_or(u8::MAX);
            let source_change_at = time_field(record, "SrcUpdateTime");
            let value = normalize_bike_availability(
                YouBikeStationId::new(station_id),
                service_status,
                u32_field(record, "AvailableRentBikes"),
                u32_field(record, "AvailableReturnBikes"),
                source_change_at,
                Some(published_at),
                received_at,
            );
            normalized.push(NormalizedLiveStatus {
                id: observation_id(&self.capability, station_uid, "bike", published_at),
                capability: self.capability.clone(),
                received_at,
                normalized_at,
                freshness: self.policy.evaluate(
                    LiveStatusTimes {
                        measured_at: source_change_at,
                        published_at: Some(published_at),
                        received_at,
                        normalized_at,
                    },
                    normalized_at,
                    None,
                ),
                value: LiveStatusValue::BikeAvailability(value),
            });
        }
        normalized
    }
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
    digest.update(b"via.live-status.v1\0");
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
    station_id: YouBikeStationId,
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

fn newest_update_time(payload: &serde_json::Value) -> Option<OffsetDateTime> {
    let mut newest = None;
    collect_update_times(payload, &mut newest);
    newest
}

fn collect_update_times(payload: &serde_json::Value, newest: &mut Option<OffsetDateTime>) {
    match payload {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_update_times(value, newest);
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "UpdateTime" | "update_time") {
                    if let Some(parsed) = value
                        .as_str()
                        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
                    {
                        if newest.is_none_or(|current| parsed > current) {
                            *newest = Some(parsed);
                        }
                    }
                } else if value.is_array() || value.is_object() {
                    collect_update_times(value, newest);
                }
            }
        }
        _ => {}
    }
}
