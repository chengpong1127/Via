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
                let earliest =
                    latest - time::Duration::seconds(i64::from(max_journey_duration_seconds));
                let now = normalize_to_taipei(now);
                let start = if latest > now {
                    earliest.max(now)
                } else {
                    earliest
                };
                (start, Some(latest))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::DepartureIntent;

    #[test]
    fn future_arrive_by_never_searches_from_the_past() {
        let now = datetime!(2026-11-02 13:22 +08:00);
        let (start, latest) = DepartureIntent::ArriveBy(datetime!(2026-11-02 14:22 +08:00))
            .search_window(now, 4 * 60 * 60);
        assert_eq!(start, now);
        assert_eq!(latest, Some(datetime!(2026-11-02 14:22 +08:00)));

        let (start, _) = DepartureIntent::ArriveBy(datetime!(2026-11-03 09:00 +08:00))
            .search_window(now, 4 * 60 * 60);
        assert_eq!(start, datetime!(2026-11-03 05:00 +08:00));

        let (start, _) = DepartureIntent::ArriveBy(datetime!(2026-08-31 10:10 +08:00))
            .search_window(now, 4 * 60 * 60);
        assert_eq!(start, datetime!(2026-08-31 06:10 +08:00));
    }
}
