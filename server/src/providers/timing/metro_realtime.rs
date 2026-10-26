use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        realtime::{FreshnessPolicy, FreshnessState, LiveStatusTimes},
        transit_timing::DepartureError,
    },
    tdx::TdxClientError,
};

pub(super) const DEFAULT_LIVE_BOARD_PATH: &str = "v2/Rail/Metro/LiveBoard/TRTC";

#[derive(Debug, Deserialize)]
pub(super) struct TdxMetroLiveBoard {
    #[serde(rename = "LineID")]
    line_id: String,
    #[serde(rename = "LineName")]
    _line_name: serde_json::Value,
    #[serde(rename = "StationID")]
    station_id: String,
    #[serde(rename = "StationName")]
    _station_name: serde_json::Value,
    #[serde(rename = "TripHeadSign")]
    _trip_head_sign: String,
    #[serde(rename = "DestinationStaionID")]
    destination_staion_id: String,
    #[serde(rename = "DestinationStationID")]
    destination_station_id: String,
    #[serde(rename = "DestinationStationName")]
    _destination_station_name: serde_json::Value,
    #[serde(rename = "ServiceStatus")]
    service_status: i32,
    #[serde(rename = "EstimateTime", default)]
    estimate_minutes: Option<i64>,
    #[serde(rename = "SrcUpdateTime")]
    source_update_time: String,
    #[serde(rename = "UpdateTime")]
    update_time: String,
}

#[derive(Clone, Debug)]
pub(super) struct MetroObservation {
    pub(super) line_id: String,
    pub(super) station_id: String,
    pub(super) destination_station_id: String,
    pub(super) departure_at: OffsetDateTime,
    pub(super) observed_at: OffsetDateTime,
}

pub(super) fn parse_realtime_payload(
    payload: Vec<TdxMetroLiveBoard>,
    freshness_policy: &FreshnessPolicy,
    max_clock_skew_seconds: u32,
) -> Result<Vec<MetroObservation>, DepartureError> {
    let received_at = OffsetDateTime::now_utc();
    let mut observations = Vec::new();
    for board in payload {
        if board.destination_station_id != board.destination_staion_id {
            return Err(DepartureError::InvalidResponse);
        }
        let source_update = parse_timestamp(&board.source_update_time)?;
        let platform_update = parse_timestamp(&board.update_time)?;
        reject_future_timestamp(source_update, received_at, max_clock_skew_seconds)?;
        reject_future_timestamp(platform_update, received_at, max_clock_skew_seconds)?;
        let observed_at = source_update.min(platform_update);
        let freshness = freshness_policy.evaluate(
            LiveStatusTimes {
                published_at: Some(observed_at),
                received_at,
            },
            received_at,
        );
        if freshness != FreshnessState::Fresh
            || board.service_status != 0
            || board.estimate_minutes.is_none_or(|minutes| minutes < 0)
        {
            continue;
        }
        let Some(estimate_minutes) = board.estimate_minutes else {
            continue;
        };
        let Some(departure_at) = observed_at.checked_add(TimeDuration::minutes(estimate_minutes))
        else {
            return Err(DepartureError::InvalidResponse);
        };
        observations.push(MetroObservation {
            line_id: board.line_id,
            station_id: board.station_id,
            destination_station_id: board.destination_station_id,
            departure_at,
            observed_at,
        });
    }
    Ok(observations)
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
        | TdxClientError::DownloadWrite(_)
        | TdxClientError::Credentials(_) => DepartureError::TemporarilyUnavailable,
        TdxClientError::ResponseTooLarge | TdxClientError::InvalidJson(_) => {
            DepartureError::InvalidResponse
        }
    }
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, DepartureError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| DepartureError::InvalidResponse)
}

fn reject_future_timestamp(
    timestamp: OffsetDateTime,
    received_at: OffsetDateTime,
    max_clock_skew_seconds: u32,
) -> Result<(), DepartureError> {
    if timestamp > received_at + TimeDuration::seconds(i64::from(max_clock_skew_seconds)) {
        return Err(DepartureError::InvalidResponse);
    }
    Ok(())
}
