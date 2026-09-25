use std::sync::Arc;

use async_trait::async_trait;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        ids::TransitPatternId,
        realtime::{
            FreshnessPolicy, LiveStatusTimes, LocalizedSourceText, NormalizedRealtimeValue,
            RealtimeValue, ServiceCondition, ServiceConditionState,
        },
    },
    providers::{
        bindings::TdxProviderBindings,
        service_conditions::{ServiceConditionError, ServiceConditionProvider},
    },
    tdx::{TdxClient, TdxClientError, tdx_sub_route_uid},
};

const MAX_PROVIDER_CLOCK_SKEW: TimeDuration = TimeDuration::minutes(5);
const TDX_AUTHORITY_NAMESPACE: &str = "tdx";

/// TDX-backed service-condition collection. Departure timing is owned by the
/// mode source and does not pass through this provider.
pub struct TdxServiceConditionProvider {
    condition_path: Option<String>,
    tdx_client: Arc<TdxClient>,
    policy: FreshnessPolicy,
    bindings: Arc<TdxProviderBindings>,
}

impl TdxServiceConditionProvider {
    pub fn new(tdx_client: Arc<TdxClient>) -> Self {
        Self {
            condition_path: None,
            tdx_client,
            policy: FreshnessPolicy::tdx_one_minute("1"),
            bindings: Arc::new(TdxProviderBindings::default()),
        }
    }

    pub fn with_bindings(mut self, bindings: Arc<TdxProviderBindings>) -> Self {
        self.bindings = bindings;
        self
    }

    pub fn with_service_condition_path(mut self, path: impl Into<String>) -> Self {
        self.condition_path = Some(path.into());
        self
    }

    async fn get_json(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<serde_json::Value, ServiceConditionError> {
        let query = query
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        self.tdx_client
            .get_json(path, &query)
            .await
            .map_err(map_tdx_error)
    }

    fn normalize_payload(
        &self,
        payload: &serde_json::Value,
        requested_services: &[TransitPatternId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError> {
        let records = records(payload);
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let received_at = OffsetDateTime::now_utc();
        let mut normalized = Vec::new();
        for record in records {
            let Some(published_at) =
                time_field(record, "UpdateTime").or_else(|| time_field(record, "update_time"))
            else {
                continue;
            };
            if published_at > received_at + MAX_PROVIDER_CLOCK_SKEW {
                return Err(ServiceConditionError::RejectedPayload);
            }
            let freshness = self.policy.evaluate(
                LiveStatusTimes {
                    measured_at: None,
                    published_at: Some(published_at),
                    received_at,
                    normalized_at: received_at,
                },
                received_at,
                None,
            );
            let Some((id, value)) =
                self.normalize_service_condition(record, published_at, requested_services)
            else {
                continue;
            };
            normalized.push(NormalizedRealtimeValue {
                id,
                received_at,
                normalized_at: received_at,
                provider_published_at: Some(published_at),
                source_change_at: None,
                freshness,
                value,
            });
        }
        if normalized.is_empty() {
            return Err(ServiceConditionError::RejectedPayload);
        }
        Ok(normalized)
    }

    fn normalize_service_condition(
        &self,
        record: &serde_json::Value,
        published_at: OffsetDateTime,
        requested_services: &[TransitPatternId],
    ) -> Option<(String, RealtimeValue)> {
        let service_uid =
            string_field(record, "SubRouteUID").or_else(|| string_field(record, "ServiceUID"))?;
        let service_ids = condition_service_ids(
            service_uid,
            direction_field(record),
            requested_services,
            &self.bindings,
        );
        if service_ids.is_empty() {
            return None;
        }
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
            scope_via_ids: service_ids,
            source_text_variants,
            effective_from: time_field(record, "EffectiveFrom"),
            effective_until: time_field(record, "EffectiveUntil"),
        };
        let condition_key = direction_field(record)
            .map(|direction| format!("condition:{direction}"))
            .unwrap_or_else(|| "condition".into());
        Some((
            observation_id(service_uid, &condition_key, published_at),
            RealtimeValue::ServiceCondition(condition),
        ))
    }
}

#[async_trait]
impl ServiceConditionProvider for TdxServiceConditionProvider {
    async fn service_conditions(
        &self,
        service_ids: &[TransitPatternId],
    ) -> Result<Vec<NormalizedRealtimeValue>, ServiceConditionError> {
        if service_ids.is_empty() {
            return Ok(Vec::new());
        }
        let Some(path) = &self.condition_path else {
            return Ok(Vec::new());
        };
        let mut service_uids = service_ids
            .iter()
            .filter_map(|id| self.bindings.bus_patterns.get(id))
            .map(|binding| {
                tdx_sub_route_uid(&TransitPatternId::new(binding.tdx_service_id.clone())).to_owned()
            })
            .collect::<Vec<_>>();
        service_uids.sort();
        service_uids.dedup();
        if service_uids.is_empty() {
            return Ok(Vec::new());
        }
        let filter = odata_in_filter("SubRouteUID", &service_uids);
        let query_params = vec![("$filter".into(), filter)];
        let payload = self.get_json(path, &query_params).await?;
        let values = self.normalize_payload(&payload, service_ids)?;
        Ok(values
            .into_iter()
            .filter(|value| {
                matches!(
                    &value.value,
                    RealtimeValue::ServiceCondition(condition)
                        if condition.scope_via_ids.iter().any(|service_id| {
                            service_ids
                                .iter()
                                .any(|requested| requested.as_str() == service_id)
                        })
                )
            })
            .collect())
    }
}

fn map_tdx_error(error: TdxClientError) -> ServiceConditionError {
    match error {
        TdxClientError::InvalidConfiguration => ServiceConditionError::InvalidConfiguration,
        TdxClientError::RateLimited | TdxClientError::RateLimitedWithRetryAfter(_) => {
            ServiceConditionError::RateLimited
        }
        TdxClientError::OAuthRequest(error) | TdxClientError::ApiRequest(error)
            if error.is_timeout() =>
        {
            ServiceConditionError::TimedOut
        }
        TdxClientError::OAuthRequest(_)
        | TdxClientError::InvalidOAuthResponse(_)
        | TdxClientError::ApiRequest(_)
        | TdxClientError::Credentials(_) => ServiceConditionError::TemporarilyUnavailable,
        TdxClientError::ResponseTooLarge | TdxClientError::InvalidJson(_) => {
            ServiceConditionError::RejectedPayload
        }
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

fn direction_field(record: &serde_json::Value) -> Option<u8> {
    record
        .get("Direction")
        .and_then(serde_json::Value::as_u64)
        .and_then(|direction| u8::try_from(direction).ok())
}

fn condition_service_ids(
    sub_route_uid: &str,
    direction: Option<u8>,
    requested_services: &[TransitPatternId],
    bindings: &TdxProviderBindings,
) -> Vec<String> {
    let mut service_ids = requested_services
        .iter()
        .filter(|pattern_id| {
            bindings
                .bus_patterns
                .get(*pattern_id)
                .is_some_and(|binding| {
                    let native = TransitPatternId::new(binding.tdx_service_id.clone());
                    tdx_sub_route_uid(&native) == sub_route_uid
                        && direction.is_none_or(|direction| {
                            binding
                                .tdx_service_id
                                .split_once("::direction=")
                                .is_none_or(|(_, native_direction)| {
                                    native_direction.parse::<u8>() == Ok(direction)
                                })
                        })
                })
        })
        .map(|service_id| service_id.as_str().to_owned())
        .collect::<Vec<_>>();
    service_ids.sort();
    service_ids.dedup();
    service_ids
}

fn time_field(record: &serde_json::Value, field: &str) -> Option<OffsetDateTime> {
    string_field(record, field).and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
}

fn observation_id(primary_key: &str, secondary_key: &str, published_at: OffsetDateTime) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"via.realtime.v1\0");
    digest.update(TDX_AUTHORITY_NAMESPACE.as_bytes());
    digest.update(primary_key.as_bytes());
    digest.update(secondary_key.as_bytes());
    digest.update(published_at.unix_timestamp_nanos().to_be_bytes());
    format!("live_{:x}", digest.finalize())
}
