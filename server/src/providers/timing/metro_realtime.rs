use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use super::realtime_check::RealtimeCheck;
use crate::{
    domain::{
        realtime::{FreshnessState, LiveStatusTimes},
        transit::timing::DepartureError,
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
    boards: Vec<MetroBoard>,
    realtime_check: &RealtimeCheck,
) -> Result<Vec<MetroObservation>, DepartureError> {
    let received_at = OffsetDateTime::now_utc();
    let mut observations = Vec::new();
    for board in boards {
        let source_update = parse_timestamp(&board.source_update_time)?;
        let platform_update = parse_timestamp(&board.update_time)?;
        realtime_check.reject_future(source_update, received_at)?;
        realtime_check.reject_future(platform_update, received_at)?;
        let observed_at = source_update.min(platform_update);
        let freshness = realtime_check.freshness.evaluate(
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
