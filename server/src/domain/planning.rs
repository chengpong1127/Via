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
    ArriveBy(OffsetDateTime),
}

impl DepartureIntent {
    pub fn search_window(
        self,
        now: OffsetDateTime,
        max_journey_duration_seconds: u32,
    ) -> (OffsetDateTime, Option<OffsetDateTime>) {
        match self {
            Self::Now => (normalize_to_taipei(now), None),
            Self::DepartAt(time) => (normalize_to_taipei(time), None),
            Self::ArriveBy(time) => {
                let latest = normalize_to_taipei(time);
                (
                    latest - time::Duration::seconds(i64::from(max_journey_duration_seconds)),
                    Some(latest),
                )
            }
        }
    }
}
