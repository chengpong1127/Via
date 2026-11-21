use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Deserialize;
use time::{Date, Weekday};

use crate::domain::transit::{
    network::{ServiceCalendar, TransitMode, TransitNetwork},
    service::{PatternHeadways, ServicePeriod},
};

pub const BUS_SCHEDULE_CITIES: &[(&str, &str)] = &[
    ("HsinchuCounty", "hsinchu-county"),
    ("KinmenCounty", "kinmen-county"),
    ("NewTaipei", "new-taipei"),
    ("PingtungCounty", "pingtung-county"),
    ("Taichung", "taichung"),
    ("Tainan", "tainan"),
    ("Taipei", "taipei"),
    ("Taoyuan", "taoyuan"),
];

#[derive(Clone, Debug, Deserialize)]
pub struct BusSchedule {
    #[serde(rename = "SubRouteUID")]
    pub sub_route_uid: String,
    #[serde(rename = "Direction")]
    pub direction: u8,
    #[serde(rename = "Timetables", default)]
    pub timetables: Vec<BusTimetable>,
    #[serde(rename = "Frequencys", default)]
    pub frequencies: Vec<BusFrequency>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BusTimetable {
    #[serde(rename = "ServiceDay")]
    pub service_day: Option<ServiceDays>,
    #[serde(rename = "StopTimes", default)]
    pub stop_times: Vec<BusStopTime>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BusStopTime {
    #[serde(rename = "DepartureTime")]
    pub departure_time: Option<String>,
    #[serde(rename = "ArrivalTime")]
    pub arrival_time: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BusFrequency {
    #[serde(rename = "StartTime")]
    pub start_time: String,
    #[serde(rename = "EndTime")]
    pub end_time: String,
    #[serde(rename = "MinHeadwayMins")]
    pub min_headway_minutes: u32,
    #[serde(rename = "MaxHeadwayMins")]
    pub max_headway_minutes: u32,
    #[serde(rename = "ServiceDay")]
    pub service_day: Option<ServiceDays>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct ServiceDays {
    #[serde(rename = "Monday", default)]
    monday: u8,
    #[serde(rename = "Tuesday", default)]
    tuesday: u8,
    #[serde(rename = "Wednesday", default)]
    wednesday: u8,
    #[serde(rename = "Thursday", default)]
    thursday: u8,
    #[serde(rename = "Friday", default)]
    friday: u8,
    #[serde(rename = "Saturday", default)]
    saturday: u8,
    #[serde(rename = "Sunday", default)]
    sunday: u8,
}

impl ServiceDays {
    fn runs_on(self, weekday: Weekday) -> bool {
        let flag = match weekday {
            Weekday::Monday => self.monday,
            Weekday::Tuesday => self.tuesday,
            Weekday::Wednesday => self.wednesday,
            Weekday::Thursday => self.thursday,
            Weekday::Friday => self.friday,
            Weekday::Saturday => self.saturday,
            Weekday::Sunday => self.sunday,
        };
        flag != 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeadwayDiagnostics {
    pub from_gtfs: usize,
    pub from_tdx: usize,
    pub without_headways: usize,
    pub unmapped_tdx_sub_routes: BTreeSet<String>,
}

#[derive(Default)]
struct ServiceCounts {
    departures: [f64; ServicePeriod::ALL.len()],
}

impl ServiceCounts {
    fn add_departure(&mut self, weekend: bool, minute: u32) {
        let period = ServicePeriod::of(weekend, minute % 1440);
        if counts_on(period, weekend) {
            self.departures[period.index()] += 1.0;
        }
    }

    fn add_frequency(&mut self, weekend: bool, start: u32, end: u32, headway: f64) {
        for period in ServicePeriod::ALL {
            if !counts_on(period, weekend) {
                continue;
            }
            let (window_start, window_end) = period.window_minutes();
            let overlap = end.min(window_end).saturating_sub(start.max(window_start));
            if overlap > 0 {
                self.departures[period.index()] += f64::from(overlap) / headway;
            }
        }
    }

    fn headways(&self) -> Vec<Option<u32>> {
        ServicePeriod::ALL
            .iter()
            .map(|period| {
                let count = self.departures[period.index()];
                let (start, end) = period.window_minutes();
                (count >= 1.0)
                    .then(|| ((f64::from(end - start) * 60.0 / count).round() as u32).max(1))
            })
            .collect()
    }
}

fn counts_on(period: ServicePeriod, weekend: bool) -> bool {
    (period == ServicePeriod::WeekendDay) == weekend
}

type Service<'a> = (&'a str, Option<u8>);

pub fn pattern_headways(
    network: &TransitNetwork,
    schedules: &[BusSchedule],
    weekday: Date,
    weekend: Date,
) -> (Vec<PatternHeadways>, HeadwayDiagnostics) {
    let mut diagnostics = HeadwayDiagnostics::default();
    let modes = network
        .routes
        .iter()
        .map(|route| (&route.id, route.mode))
        .collect::<HashMap<_, _>>();
    let calendars = network
        .calendars
        .iter()
        .map(|calendar| (&calendar.id, calendar))
        .collect::<HashMap<_, _>>();
    let service_by_pattern = network
        .patterns
        .iter()
        .filter(|pattern| {
            matches!(
                modes.get(&pattern.route_id),
                Some(TransitMode::Bus | TransitMode::Metro)
            )
        })
        .map(|pattern| (&pattern.id, (pattern.route_id.as_str(), pattern.direction)))
        .collect::<HashMap<_, _>>();
    let runs_on = |calendar_id, day| {
        calendars
            .get(calendar_id)
            .is_some_and(|calendar: &&ServiceCalendar| calendar.runs_on(day))
    };
    let mut gtfs_counts = HashMap::<Service, ServiceCounts>::new();
    for trip in &network.trips {
        let Some(service) = service_by_pattern.get(&trip.pattern_id).copied() else {
            continue;
        };
        let counts = gtfs_counts.entry(service).or_default();
        let Some(first) = trip.stop_times.first() else {
            continue;
        };
        for (day, is_weekend) in [(weekday, false), (weekend, true)] {
            if runs_on(&trip.calendar_id, day) {
                counts.add_departure(is_weekend, first.departure_seconds / 60);
            }
        }
    }
    for frequency in &network.frequencies {
        let Some(service) = service_by_pattern.get(&frequency.pattern_id).copied() else {
            continue;
        };
        let counts = gtfs_counts.entry(service).or_default();
        for (day, is_weekend) in [(weekday, false), (weekend, true)] {
            if runs_on(&frequency.calendar_id, day) {
                counts.add_frequency(
                    is_weekend,
                    frequency.start_seconds / 60,
                    frequency.end_seconds / 60,
                    f64::from(frequency.headway_seconds) / 60.0,
                );
            }
        }
    }

    let services = service_by_pattern.values().copied().collect::<HashSet<_>>();
    let mut tdx_counts = HashMap::<Service, ServiceCounts>::new();
    for schedule in schedules {
        let service = (schedule.sub_route_uid.as_str(), Some(schedule.direction));
        if !services.contains(&service) {
            diagnostics
                .unmapped_tdx_sub_routes
                .insert(format!("{}:{}", schedule.sub_route_uid, schedule.direction));
            continue;
        }
        if gtfs_counts.contains_key(&service) {
            continue;
        }
        let counts = tdx_counts.entry(service).or_default();
        for (day, is_weekend) in [(weekday, false), (weekend, true)] {
            for timetable in &schedule.timetables {
                if !timetable
                    .service_day
                    .is_some_and(|days| days.runs_on(day.weekday()))
                {
                    continue;
                }
                if let Some(minute) = timetable.stop_times.first().and_then(|stop_time| {
                    stop_time
                        .departure_time
                        .as_deref()
                        .or(stop_time.arrival_time.as_deref())
                        .and_then(clock_minutes)
                }) {
                    counts.add_departure(is_weekend, minute);
                }
            }
            for frequency in &schedule.frequencies {
                if !frequency
                    .service_day
                    .is_some_and(|days| days.runs_on(day.weekday()))
                {
                    continue;
                }
                let (Some(start), Some(end)) = (
                    clock_minutes(&frequency.start_time),
                    clock_minutes(&frequency.end_time),
                ) else {
                    continue;
                };
                let headway =
                    f64::from(frequency.min_headway_minutes + frequency.max_headway_minutes) / 2.0;
                if headway > 0.0 && end > start {
                    counts.add_frequency(is_weekend, start, end, headway);
                }
            }
        }
    }

    let mut headways = Vec::new();
    for pattern in &network.patterns {
        let service = service_by_pattern.get(&pattern.id);
        let counts = if let Some(counts) = service.and_then(|service| gtfs_counts.get(service)) {
            diagnostics.from_gtfs += 1;
            counts
        } else if let Some(counts) = service.and_then(|service| tdx_counts.get(service)) {
            diagnostics.from_tdx += 1;
            counts
        } else {
            diagnostics.without_headways += 1;
            continue;
        };
        headways.push(PatternHeadways {
            pattern_id: pattern.id.clone(),
            seconds_by_period: counts.headways(),
        });
    }
    (headways, diagnostics)
}

fn clock_minutes(value: &str) -> Option<u32> {
    let (hours, minutes) = value.trim().split_once(':')?;
    let hours = hours.parse::<u32>().ok()?;
    let minutes = minutes.get(..2).unwrap_or(minutes).parse::<u32>().ok()?;
    (minutes < 60).then_some(hours * 60 + minutes)
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::{BusSchedule, clock_minutes, pattern_headways};
    use crate::domain::{
        ids::{BoardingPointId, TransitPatternId, TransitRouteId},
        location::GeoCoordinate,
        transit::network::{
            SegmentProfile, SegmentTimeSource, SegmentTravelTime, TransitMode, TransitNetwork,
            TransitPattern, TransitRoute, TransitStop,
        },
    };

    fn network() -> TransitNetwork {
        let stops = ["A", "B"]
            .map(|id| {
                TransitStop::new(
                    BoardingPointId::new(id),
                    id,
                    GeoCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                )
            })
            .to_vec();
        let profile = SegmentProfile {
            segments: vec![SegmentTravelTime {
                seconds: 60,
                source: SegmentTimeSource::Estimated,
            }],
        };
        let patterns = [("TPE1", 0), ("TPE2", 1)]
            .map(|(route, direction)| {
                TransitPattern::new(
                    TransitPatternId::new(format!("pattern:{route}::direction={direction}")),
                    TransitRouteId::new(route),
                    Some(direction),
                    "",
                    vec![BoardingPointId::new("A"), BoardingPointId::new("B")],
                    profile.clone(),
                )
            })
            .to_vec();
        TransitNetwork::new(
            crate::domain::transit::network::default_service_day_boundary(),
            stops,
            patterns,
            Vec::new(),
        )
        .with_routes(
            ["TPE1", "TPE2"]
                .map(|id| TransitRoute {
                    id: TransitRouteId::new(id),
                    mode: TransitMode::Bus,
                    label: id.to_owned(),
                })
                .to_vec(),
        )
    }

    #[test]
    fn variants_of_one_route_direction_share_a_combined_headway() {
        use crate::domain::ids::TransitTripId;
        use crate::domain::transit::network::{ScheduledStopTime, TransitTrip};
        use time::Weekday;

        let stops = ["o", "a", "b"]
            .map(|id| {
                TransitStop::new(
                    BoardingPointId::new(id),
                    id,
                    GeoCoordinate {
                        latitude: 25.0,
                        longitude: 121.5,
                    },
                )
            })
            .to_vec();
        let variant = |id: &str, stops: &[&str]| {
            TransitPattern::fixture(
                TransitPatternId::new(id),
                TransitMode::Metro,
                "BL",
                "",
                stops
                    .iter()
                    .map(|stop| BoardingPointId::new(*stop))
                    .collect(),
            )
        };
        let trips = (0..12)
            .map(|index| {
                let start = 7 * 3600 + index * 600;
                TransitTrip::fixture_weekdays(
                    TransitTripId::new(format!("trip-{index}")),
                    TransitPatternId::new(if index < 6 { "full" } else { "short" }),
                    vec![Weekday::Wednesday, Weekday::Saturday],
                    (0..if index < 6 { 3 } else { 2 })
                        .map(|stop| ScheduledStopTime::new(start + stop * 60, start + stop * 60))
                        .collect(),
                )
            })
            .collect::<Vec<_>>();
        let network = TransitNetwork::fixture(
            crate::domain::transit::network::default_service_day_boundary(),
            stops,
            vec![
                variant("full", &["o", "a", "b"]),
                variant("short", &["o", "a"]),
            ],
            trips,
        );
        let (headways, diagnostics) =
            pattern_headways(&network, &[], date!(2026 - 11 - 25), date!(2026 - 11 - 28));
        assert_eq!(diagnostics.from_gtfs, 2);
        assert!(headways.iter().all(|headways| {
            headways.seconds_by_period == vec![None, Some(900), None, None, None, Some(5400)]
        }));
    }

    #[test]
    fn clock_minutes_reads_tdx_clock_strings() {
        assert_eq!(clock_minutes("05:10"), Some(310));
        assert_eq!(clock_minutes("24:30"), Some(1470));
        assert_eq!(clock_minutes("7:61"), None);
        assert_eq!(clock_minutes("x"), None);
    }

    #[test]
    fn tdx_schedules_give_headways_per_service_period() {
        let weekdays = r#"{"Monday":1,"Tuesday":1,"Wednesday":1,"Thursday":1,"Friday":1,"Saturday":0,"Sunday":0}"#;
        let saturday = r#"{"Monday":0,"Tuesday":0,"Wednesday":0,"Thursday":0,"Friday":0,"Saturday":1,"Sunday":0}"#;
        let schedules: Vec<BusSchedule> = serde_json::from_str(&format!(
            r#"[
              {{"SubRouteUID":"TPE1","Direction":0,
                "Frequencys":[{{"StartTime":"06:00","EndTime":"09:00","MinHeadwayMins":8,"MaxHeadwayMins":12,"ServiceDay":{weekdays}}}],
                "Timetables":[{{"ServiceDay":{saturday},"StopTimes":[{{"DepartureTime":"10:00"}}]}},
                              {{"ServiceDay":{saturday},"StopTimes":[{{"DepartureTime":"12:00"}}]}}]}},
              {{"SubRouteUID":"TPE9","Direction":0,"Timetables":[]}}
            ]"#
        ))
        .unwrap();
        let network = network();
        let (headways, diagnostics) = pattern_headways(
            &network,
            &schedules,
            date!(2026 - 11 - 25),
            date!(2026 - 11 - 28),
        );
        assert_eq!(headways.len(), 1);
        assert_eq!(
            headways[0].seconds_by_period,
            vec![None, Some(600), None, None, None, Some(32_400)]
        );
        assert_eq!(diagnostics.from_tdx, 1);
        assert_eq!(diagnostics.without_headways, 1);
        assert_eq!(
            diagnostics
                .unmapped_tdx_sub_routes
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["TPE9:0".to_owned()]
        );
    }
}
