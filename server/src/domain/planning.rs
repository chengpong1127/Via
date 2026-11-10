use time::OffsetDateTime;

use super::{location::GeoCoordinate, normalize_to_taipei, transit::TransitMode};

#[derive(Clone, Debug, PartialEq)]
pub struct PlanningInput {
    pub origin: PlanningEndpoint,
    pub destination: PlanningEndpoint,
    pub transit_modes: Vec<TransitMode>,
    pub bike_share: bool,
    pub departure: DepartureIntent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanningEndpoint {
    pub coordinate: GeoCoordinate,
    pub display_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepartureIntent {
    Now,
    DepartAt(OffsetDateTime),
}

impl DepartureIntent {
    pub fn search_start(self, now: OffsetDateTime) -> OffsetDateTime {
        match self {
            Self::Now => normalize_to_taipei(now),
            Self::DepartAt(time) => normalize_to_taipei(time),
        }
    }
}
