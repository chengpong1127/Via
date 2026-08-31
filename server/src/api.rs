use std::{fmt, str::FromStr};

use language_tags::LanguageTag;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use unicode_normalization::UnicodeNormalization;

use crate::street::Coordinate;

pub const API_VERSION: &str = "1.0";
pub const SEMANTIC_CATALOG_VERSION: &str = "1.0";

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlanningRequest {
    pub origin: SelectedPlace,
    pub destination: SelectedPlace,
    pub selected_modes: Vec<SelectedMode>,
    pub locale: String,
    pub departure_setting: DepartureSetting,
}

impl PlanningRequest {
    pub fn validate(&self) -> Result<(), RequestValidationError> {
        self.origin.validate("/origin")?;
        self.destination.validate("/destination")?;
        if self.selected_modes.is_empty()
            || self.selected_modes.contains(&SelectedMode::Unknown)
            || self
                .selected_modes
                .windows(2)
                .any(|modes| modes[0] >= modes[1])
        {
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
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SelectedPlace {
    pub coordinate: RequestCoordinate,
    pub display_text: String,
}

impl SelectedPlace {
    fn validate(&self, pointer: &str) -> Result<(), RequestValidationError> {
        self.coordinate.validate(pointer)?;
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RequestCoordinate {
    pub latitude: f64,
    pub longitude: f64,
}

impl RequestCoordinate {
    fn validate(&self, pointer: &str) -> Result<(), RequestValidationError> {
        if !self.latitude.is_finite() || !(-90.0..=90.0).contains(&self.latitude) {
            return Err(RequestValidationError::new(
                "invalid_coordinate",
                format!("{pointer}/coordinate/latitude"),
            ));
        }
        if !self.longitude.is_finite() || !(-180.0..=180.0).contains(&self.longitude) {
            return Err(RequestValidationError::new(
                "invalid_coordinate",
                format!("{pointer}/coordinate/longitude"),
            ));
        }
        Ok(())
    }
}

impl TryFrom<RequestCoordinate> for Coordinate {
    type Error = RequestValidationError;

    fn try_from(value: RequestCoordinate) -> Result<Self, Self::Error> {
        Coordinate::try_new(value.latitude, value.longitude)
            .map_err(|_| RequestValidationError::new("invalid_coordinate", "/coordinate"))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SelectedMode {
    Bus,
    Metro,
    #[serde(rename = "youbike")]
    YouBike,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
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
            Self::DepartNow => now,
            Self::DepartAt { time } | Self::ArriveBy { time } => *time,
        }
    }

    pub fn resolve(
        &self,
        now: OffsetDateTime,
        duration_seconds: u32,
    ) -> (OffsetDateTime, OffsetDateTime) {
        let duration = time::Duration::seconds(i64::from(duration_seconds));
        match self {
            Self::DepartNow => (now, now + duration),
            Self::DepartAt { time } => (*time, *time + duration),
            Self::ArriveBy { time } => (*time - duration, *time),
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
