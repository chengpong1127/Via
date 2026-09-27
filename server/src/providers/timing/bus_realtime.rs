use std::collections::HashMap;

use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        ids::{BoardingPointId, TransitPatternId},
        realtime::{
            ArrivalEstimate, ArrivalState, FreshnessPolicy, LiveStatusTimes,
            NormalizedRealtimeValue, RealtimeValue,
        },
        transit_timing::DepartureError,
    },
    tdx::{
        TdxClientError, boarding_point_id, transit_service_id, transit_service_id_for_direction,
    },
};

pub(super) const DEFAULT_ARRIVAL_PATH: &str = "v2/Bus/EstimatedTimeOfArrival/City";
const MAX_PROVIDER_CLOCK_SKEW: TimeDuration = TimeDuration::minutes(5);
const TDX_AUTHORITY_NAMESPACE: &str = "tdx";

#[derive(Debug, Deserialize)]
struct TdxBusEstimatedArrival {
    #[serde(rename = "StopUID")]
    stop_uid: String,
    #[serde(rename = "SubRouteUID", default)]
    sub_route_uid: Option<String>,
    #[serde(rename = "RouteUID", default)]
    route_uid: Option<String>,
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
    requested_services: &HashMap<String, Vec<TransitPatternId>>,
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
        let Some(service_uid) = record
            .sub_route_uid
            .as_deref()
            .or(record.route_uid.as_deref())
        else {
            continue;
        };
        let directional_id = record
            .direction
            .map(|direction| transit_service_id_for_direction(service_uid, direction));
        let base_id = transit_service_id(service_uid);
        let ids = directional_id
            .as_ref()
            .and_then(|id| requested_services.get(id.as_str()))
            .or_else(|| requested_services.get(base_id.as_str()));
        for pattern_id in ids.into_iter().flatten() {
            let arrival = normalize_n1(
                pattern_id.clone(),
                boarding_point_id(record.stop_uid.clone()),
                record.stop_status.unwrap_or(u8::MAX),
                record.estimate_seconds,
                published_at,
            );
            normalized.push(NormalizedRealtimeValue {
                id: observation_id(&record.stop_uid, service_uid, published_at),
                received_at,
                normalized_at: received_at,
                provider_published_at: Some(published_at),
                source_change_at: None,
                freshness: freshness.clone(),
                value: RealtimeValue::ArrivalEstimate(arrival),
            });
        }
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
    transit_pattern_id: TransitPatternId,
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
        transit_pattern_id,
        boarding_point_id,
        predicted_arrival_at,
        state,
        transmitted_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::format_description::well_known::Rfc3339;

    #[test]
    fn route_uid_fallback_maps_tdx_bus_estimates_without_subroute_uid() {
        let pattern_id = TransitPatternId::new("pattern:TPE11871::direction=0::variant=0");
        let service_id = transit_service_id_for_direction("TPE11871", 0);
        let requested_services =
            HashMap::from([(service_id.as_str().to_owned(), vec![pattern_id.clone()])]);
        let update_time = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
        let payload = serde_json::json!([{
            "RouteUID": "TPE11871",
            "StopUID": "TPE11896",
            "Direction": 0,
            "StopStatus": 0,
            "EstimateTime": 1381,
            "UpdateTime": update_time,
        }]);

        let values = parse_bus_realtime_payload(
            &payload,
            &requested_services,
            &FreshnessPolicy::tdx_one_minute("test"),
        )
        .unwrap();

        assert_eq!(values.len(), 1);
        let RealtimeValue::ArrivalEstimate(arrival) = &values[0].value else {
            panic!("expected an arrival estimate");
        };
        assert_eq!(arrival.transit_pattern_id, pattern_id);
        assert_eq!(arrival.boarding_point_id, boarding_point_id("TPE11896"));
        assert_eq!(arrival.state, ArrivalState::Predicted);
    }
}
