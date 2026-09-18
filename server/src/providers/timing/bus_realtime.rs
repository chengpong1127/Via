use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    config::TdxClientError,
    domain::{
        ids::{BoardingPointId, TransitServiceId},
        realtime::{
            ArrivalEstimate, ArrivalState, FreshnessPolicy, LiveStatusTimes,
            NormalizedRealtimeValue, RealtimeValue,
        },
        transit_timing::DepartureError,
    },
    providers::tdx::transit_service_id_for_direction,
};

pub(super) const DEFAULT_ARRIVAL_PATH: &str = "v2/Bus/EstimatedTimeOfArrival/City";
const MAX_PROVIDER_CLOCK_SKEW: TimeDuration = TimeDuration::minutes(5);
const DIRECTIONAL_SERVICE_SEPARATOR: &str = "::direction=";
const TDX_AUTHORITY_NAMESPACE: &str = "tdx";

#[derive(Debug, Deserialize)]
struct TdxBusEstimatedArrival {
    #[serde(rename = "StopUID")]
    stop_uid: String,
    #[serde(rename = "SubRouteUID")]
    sub_route_uid: String,
    #[serde(rename = "Direction", default)]
    direction: Option<u8>,
    #[serde(rename = "StopStatus", default)]
    stop_status: Option<u8>,
    #[serde(rename = "EstimateTime", default)]
    estimate_seconds: Option<i64>,
    #[serde(rename = "UpdateTime")]
    update_time: String,
}

pub(super) fn parse_bus_realtime_payload(
    payload: &serde_json::Value,
    requested_services: &[TransitServiceId],
    freshness_policy: &FreshnessPolicy,
) -> Result<Vec<NormalizedRealtimeValue>, DepartureError> {
    let records = payload
        .as_array()
        .or_else(|| payload.get("value").and_then(serde_json::Value::as_array))
        .ok_or(DepartureError::InvalidResponse)?;
    let received_at = OffsetDateTime::now_utc();
    let mut normalized = Vec::new();
    for record in records {
        let Ok(record) = serde_json::from_value::<TdxBusEstimatedArrival>(record.clone()) else {
            continue;
        };
        let Ok(published_at) = OffsetDateTime::parse(&record.update_time, &Rfc3339) else {
            continue;
        };
        if published_at > received_at + MAX_PROVIDER_CLOCK_SKEW {
            return Err(DepartureError::InvalidResponse);
        }
        let freshness = freshness_policy.evaluate(
            LiveStatusTimes {
                measured_at: None,
                published_at: Some(published_at),
                received_at,
                normalized_at: received_at,
            },
            received_at,
            None,
        );
        let service_id = match record.direction {
            Some(direction)
                if requested_services.iter().any(|requested| {
                    tdx_sub_route_uid(requested) == record.sub_route_uid
                        && requested.as_str()
                            == transit_service_id_for_direction(&record.sub_route_uid, direction)
                                .as_str()
                }) =>
            {
                transit_service_id_for_direction(&record.sub_route_uid, direction)
            }
            _ => crate::providers::tdx::transit_service_id(&record.sub_route_uid),
        };
        let arrival = normalize_n1(
            service_id,
            crate::providers::tdx::boarding_point_id(record.stop_uid.clone()),
            record.stop_status.unwrap_or(u8::MAX),
            record.estimate_seconds,
            published_at,
        );
        normalized.push(NormalizedRealtimeValue {
            id: observation_id(&record.stop_uid, &record.sub_route_uid, published_at),
            received_at,
            normalized_at: received_at,
            provider_published_at: Some(published_at),
            source_change_at: None,
            freshness,
            value: RealtimeValue::ArrivalEstimate(arrival),
        });
    }
    if !records.is_empty() && normalized.is_empty() {
        return Err(DepartureError::InvalidResponse);
    }
    Ok(normalized)
}

pub(super) fn map_tdx_error(error: TdxClientError) -> DepartureError {
    match error {
        TdxClientError::InvalidConfiguration => DepartureError::InvalidConfiguration,
        TdxClientError::RateLimited | TdxClientError::RateLimitedWithRetryAfter(_) => {
            DepartureError::RateLimited
        }
        TdxClientError::OAuthRequest(error) | TdxClientError::ApiRequest(error)
            if error.is_timeout() =>
        {
            DepartureError::TimedOut
        }
        TdxClientError::OAuthRequest(_)
        | TdxClientError::InvalidOAuthResponse(_)
        | TdxClientError::ApiRequest(_)
        | TdxClientError::Credentials(_) => DepartureError::TemporarilyUnavailable,
        TdxClientError::ResponseTooLarge | TdxClientError::InvalidJson(_) => {
            DepartureError::InvalidResponse
        }
    }
}

pub(super) fn tdx_sub_route_uid(service_id: &TransitServiceId) -> &str {
    service_id
        .as_str()
        .split_once(DIRECTIONAL_SERVICE_SEPARATOR)
        .map_or_else(|| service_id.as_str(), |(sub_route_uid, _)| sub_route_uid)
}

pub(super) fn odata_in_filter(field: &str, values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!("{field} in ({values})")
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
                Some(transmitted_at + TimeDuration::seconds(seconds)),
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
