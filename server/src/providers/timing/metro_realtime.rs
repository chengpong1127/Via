use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{
        realtime::{FreshnessPolicy, FreshnessState, LiveStatusTimes},
        transit_timing::DepartureError,
    },
    live::MetroBoard,
};

#[derive(Clone, Debug)]
pub(super) struct MetroObservation {
    pub(super) line_id: String,
    pub(super) station_id: String,
    pub(super) destination_station_id: String,
    pub(super) departure_at: OffsetDateTime,
    pub(super) observed_at: OffsetDateTime,
}

pub(super) fn parse_metro_boards(
    payload: Vec<MetroBoard>,
    freshness_policy: &FreshnessPolicy,
    max_clock_skew_seconds: u32,
) -> Result<Vec<MetroObservation>, DepartureError> {
    let received_at = OffsetDateTime::now_utc();
    let mut observations = Vec::new();
    for board in payload {
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
