use std::{fmt, str::FromStr};

use language_tags::LanguageTag;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};
use unicode_normalization::UnicodeNormalization;

use crate::domain::transit::TransitMode as DomainTransitMode;

pub use crate::domain::location::GeoCoordinate;

pub const API_VERSION: &str = "1.0";
pub const SEMANTIC_CATALOG_VERSION: &str = "1.0";
pub const TAIPEI_TIME_ZONE: &str = "Asia/Taipei";

pub use crate::domain::{normalize_to_taipei, taipei_offset};

pub fn new_request_id() -> String {
    format!("req_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanningRequest {
    pub origin: SelectedPlace,
    pub destination: SelectedPlace,
    #[schema(min_items = 1)]
    pub selected_modes: Vec<PlanningMode>,
    #[schema(pattern = "^[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$")]
    pub locale: String,
    pub departure_setting: DepartureSetting,
}

impl PlanningRequest {
    pub fn validate(&self) -> Result<(), RequestValidationError> {
        self.origin.validate("/origin")?;
        self.destination.validate("/destination")?;
        let mut modes = self.selected_modes.clone();
        modes.sort();
        if modes.is_empty() || modes.windows(2).any(|modes| modes[0] == modes[1]) {
            return Err(RequestValidationError::new(
                "invalid_selected_modes",
                "/selected_modes",
            ));
        }
        LanguageTag::from_str(&self.locale)
            .map_err(|_| RequestValidationError::new("invalid_locale", "/locale"))?;
        self.departure_setting.validate()?;
        Ok(())
    }

    pub fn allowed_transit_modes(&self) -> Vec<DomainTransitMode> {
        let mut modes = self
            .selected_modes
            .iter()
            .filter_map(|mode| mode.transit_mode())
            .collect::<Vec<_>>();
        modes.sort();
        modes.dedup();
        modes
    }

    pub fn canonical_selected_modes(&self) -> Vec<PlanningMode> {
        let mut modes = self.selected_modes.clone();
        modes.sort();
        modes
    }

    pub fn allows_bike_share(&self) -> bool {
        self.selected_modes.contains(&PlanningMode::BikeShare)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectedPlace {
    pub coordinate: GeoCoordinate,
    #[schema(min_length = 1, max_length = 200)]
    pub display_text: String,
}

impl SelectedPlace {
    fn validate(&self, pointer: &str) -> Result<(), RequestValidationError> {
        if !self.coordinate.latitude.is_finite()
            || !(-90.0..=90.0).contains(&self.coordinate.latitude)
        {
            return Err(RequestValidationError::new(
                "invalid_coordinate",
                format!("{pointer}/coordinate/latitude"),
            ));
        }
        if !self.coordinate.longitude.is_finite()
            || !(-180.0..=180.0).contains(&self.coordinate.longitude)
        {
            return Err(RequestValidationError::new(
                "invalid_coordinate",
                format!("{pointer}/coordinate/longitude"),
            ));
        }
        if self.display_text.trim() != self.display_text
            || self.display_text.is_empty()
            || self.display_text.chars().count() > 200
            || !self.display_text.chars().eq(self.display_text.nfc())
        {
            return Err(RequestValidationError::new(
                "invalid_display_text",
                format!("{pointer}/display_text"),
            ));
        }
        Ok(())
    }
}

pub use crate::domain::journey::LegMode;

#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[schema(as = TransportMode)]
pub enum PlanningMode {
    Bus,
    Metro,
    Tra,
    Thsr,
    BikeShare,
}

impl PlanningMode {
    fn transit_mode(self) -> Option<DomainTransitMode> {
        match self {
            Self::Bus => Some(DomainTransitMode::Bus),
            Self::Metro => Some(DomainTransitMode::Metro),
            Self::Tra => Some(DomainTransitMode::Rail),
            Self::Thsr => Some(DomainTransitMode::HighSpeedRail),
            Self::BikeShare => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DepartureSetting {
    DepartNow,
    DepartAt {
        #[serde(with = "time::serde::rfc3339")]
        time: OffsetDateTime,
    },
    ArriveBy {
        #[serde(with = "time::serde::rfc3339")]
        time: OffsetDateTime,
    },
}

impl DepartureSetting {
    fn validate(&self) -> Result<(), RequestValidationError> {
        match self {
            Self::DepartNow => Ok(()),
            Self::DepartAt { time } | Self::ArriveBy { time }
                if time.offset().whole_seconds().abs() <= 86_399 =>
            {
                Ok(())
            }
            _ => Err(RequestValidationError::new(
                "invalid_departure_setting",
                "/departure_setting/time",
            )),
        }
    }

    pub fn effective_search_time(&self, now: OffsetDateTime) -> OffsetDateTime {
        match self {
            Self::DepartNow => normalize_to_taipei(now),
            Self::DepartAt { time } | Self::ArriveBy { time } => normalize_to_taipei(*time),
        }
    }

    pub fn local_search_time(&self, now: OffsetDateTime) -> PrimitiveDateTime {
        let effective = self.effective_search_time(now);
        effective.date().with_time(effective.time())
    }

    pub fn service_day(&self, now: OffsetDateTime, boundary: Time) -> Date {
        let local = self.local_search_time(now);
        if local.time() < boundary {
            local.date().previous_day().unwrap_or(local.date())
        } else {
            local.date()
        }
    }

    pub fn resolve(
        &self,
        now: OffsetDateTime,
        duration_seconds: u32,
    ) -> (OffsetDateTime, OffsetDateTime) {
        let duration = time::Duration::seconds(i64::from(duration_seconds));
        match self {
            Self::DepartNow => {
                let now = normalize_to_taipei(now);
                (now, now + duration)
            }
            Self::DepartAt { time } => {
                let time = normalize_to_taipei(*time);
                (time, time + duration)
            }
            Self::ArriveBy { time } => {
                let time = normalize_to_taipei(*time);
                (time - duration, time)
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RequestValidationError {
    code: String,
    field: String,
}

impl RequestValidationError {
    pub fn new(code: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            field: field.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn field(&self) -> &str {
        &self.field
    }
}

impl fmt::Display for RequestValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.code, self.field)
    }
}

impl std::error::Error for RequestValidationError {}
