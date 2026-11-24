use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, Weekday};

use crate::domain::{ids::TransitPatternId, taipei_offset};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePeriod {
    Night,
    WeekdayMorningPeak,
    WeekdayDay,
    WeekdayEveningPeak,
    WeekdayEvening,
    WeekendDay,
}

impl ServicePeriod {
    pub const ALL: [ServicePeriod; 6] = [
        ServicePeriod::Night,
        ServicePeriod::WeekdayMorningPeak,
        ServicePeriod::WeekdayDay,
        ServicePeriod::WeekdayEveningPeak,
        ServicePeriod::WeekdayEvening,
        ServicePeriod::WeekendDay,
    ];

    pub fn of(weekend: bool, minute_of_day: u32) -> Self {
        let hour = minute_of_day / 60;
        match (weekend, hour) {
            (_, 0..=5) => Self::Night,
            (true, _) => Self::WeekendDay,
            (false, 6..=8) => Self::WeekdayMorningPeak,
            (false, 9..=15) => Self::WeekdayDay,
            (false, 16..=18) => Self::WeekdayEveningPeak,
            (false, _) => Self::WeekdayEvening,
        }
    }

    pub fn at(instant: OffsetDateTime) -> Self {
        let local = instant.to_offset(taipei_offset());
        let weekend = matches!(local.weekday(), Weekday::Saturday | Weekday::Sunday);
        Self::of(
            weekend,
            u32::from(local.hour()) * 60 + u32::from(local.minute()),
        )
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn window_minutes(self) -> (u32, u32) {
        match self {
            Self::Night => (0, 360),
            Self::WeekdayMorningPeak => (360, 540),
            Self::WeekdayDay => (540, 960),
            Self::WeekdayEveningPeak => (960, 1140),
            Self::WeekdayEvening => (1140, 1440),
            Self::WeekendDay => (360, 1440),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternHeadways {
    pub pattern_id: TransitPatternId,
    pub seconds_by_period: Vec<Option<u32>>,
}

#[cfg(test)]
mod tests {
    use super::ServicePeriod;
    use time::macros::datetime;

    #[test]
    fn service_periods_follow_local_weekday_and_hour() {
        assert_eq!(
            ServicePeriod::at(datetime!(2026-11-23 08:30 +08:00)),
            ServicePeriod::WeekdayMorningPeak
        );
        assert_eq!(
            ServicePeriod::at(datetime!(2026-11-23 12:00 +08:00)),
            ServicePeriod::WeekdayDay
        );
        assert_eq!(
            ServicePeriod::at(datetime!(2026-11-28 12:00 +08:00)),
            ServicePeriod::WeekendDay
        );
        assert_eq!(
            ServicePeriod::at(datetime!(2026-11-28 03:00 +08:00)),
            ServicePeriod::Night
        );
        assert_eq!(
            ServicePeriod::at(datetime!(2026-11-23 00:30 UTC)),
            ServicePeriod::WeekdayMorningPeak
        );
        for (index, period) in ServicePeriod::ALL.iter().enumerate() {
            assert_eq!(period.index(), index);
        }
    }
}
