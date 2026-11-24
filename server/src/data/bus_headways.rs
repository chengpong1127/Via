use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Deserialize;
use time::{Date, OffsetDateTime, Weekday};

use crate::{
    domain::taipei_offset,
    domain::transit::{
        network::{ServiceCalendar, TransitMode, TransitNetwork},
        service::{PatternHeadways, ServicePeriod},
    },
    providers::tdx_bindings::TdxProviderBindings,
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

pub const BUS_SCHEDULE_FIELDS: &str = "SubRouteUID,RouteUID,Direction,Timetables,Frequencys";

pub fn is_bus_schedule_source(source_name: &str) -> bool {
    BUS_SCHEDULE_CITIES
        .iter()
        .any(|(_, slug)| source_name == format!("bus-schedule-{slug}"))
}

pub fn estimate_service_days(fetched_at: OffsetDateTime) -> (Date, Date) {
    let today = fetched_at.to_offset(taipei_offset()).date();
    let next = |weekday: Weekday| {
        let ahead = (i64::from(weekday.number_days_from_monday())
            - i64::from(today.weekday().number_days_from_monday()))
        .rem_euclid(7);
        today + time::Duration::days(ahead)
    };
    (next(Weekday::Wednesday), next(Weekday::Saturday))
}

#[derive(Clone, Debug, Deserialize)]
pub struct BusSchedule {
    #[serde(rename = "SubRouteUID")]
    pub sub_route_uid: String,
    #[serde(rename = "RouteUID", default)]
    pub route_uid: Option<String>,
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
            let overlap = [0, 1440]
                .into_iter()
                .map(|shift| {
                    end.min(window_end + shift)
                        .saturating_sub(start.max(window_start + shift))
                })
                .sum::<u32>();
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
                let end = if end <= start { end + 1440 } else { end };
                if headway > 0.0 {
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

pub fn attach_estimate_headways(
    bindings: &mut TdxProviderBindings,
    network: &TransitNetwork,
    headways: &[PatternHeadways],
) -> usize {
    let scheduled = network
        .trips
        .iter()
        .map(|trip| &trip.pattern_id)
        .chain(
            network
                .frequencies
                .iter()
                .map(|frequency| &frequency.pattern_id),
        )
        .collect::<HashSet<_>>();
    let headways_by_pattern = headways
        .iter()
        .filter(|headways| !scheduled.contains(&headways.pattern_id))
        .map(|headways| (&headways.pattern_id, &headways.seconds_by_period))
        .collect::<HashMap<_, _>>();
    let mut attached = 0;
    for (pattern_id, binding) in &mut bindings.bus_patterns {
        binding.headway_seconds_by_period = headways_by_pattern
            .get(pattern_id)
            .map(|seconds| (*seconds).clone())
            .unwrap_or_default();
        attached += usize::from(!binding.headway_seconds_by_period.is_empty());
    }
    attached
}

#[cfg(test)]
mod tests {
    use time::{Weekday, macros::date};

    use super::*;
    use crate::domain::{
        ids::{
            BoardingPointId, ServiceCalendarId, TransitPatternId, TransitRouteId, TransitTripId,
        },
        transit::network::{
            ScheduledStopTime, SegmentProfile, TransitPattern, TransitRoute, TransitTrip,
        },
    };
    use crate::providers::tdx_bindings::BusPatternBinding;

    const WEEKDAY: Date = date!(2026 - 11 - 25);
    const WEEKEND: Date = date!(2026 - 11 - 28);

    fn pattern(id: &str, route: &str) -> TransitPattern {
        TransitPattern::new(
            TransitPatternId::new(id),
            TransitRouteId::new(route),
            Some(0),
            "Terminal",
            vec![BoardingPointId::new("a"), BoardingPointId::new("b")],
            SegmentProfile {
                segments: Vec::new(),
            },
        )
    }

    fn network() -> TransitNetwork {
        let every_day = ServiceCalendar {
            id: ServiceCalendarId::new("daily"),
            weekdays: vec![
                Weekday::Monday,
                Weekday::Tuesday,
                Weekday::Wednesday,
                Weekday::Thursday,
                Weekday::Friday,
                Weekday::Saturday,
                Weekday::Sunday,
            ],
            start_date: None,
            end_date: None,
            added_dates: Vec::new(),
            removed_dates: Vec::new(),
        };
        let trips = (9..15)
            .map(|hour| {
                TransitTrip::new(
                    TransitTripId::new(format!("gtfs-{hour}")),
                    TransitPatternId::new("gtfs-pattern"),
                    ServiceCalendarId::new("daily"),
                    vec![
                        ScheduledStopTime::new(hour * 3600, hour * 3600),
                        ScheduledStopTime::new(hour * 3600 + 300, hour * 3600 + 300),
                    ],
                )
            })
            .collect();
        TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            vec![
                pattern("gtfs-pattern", "GTFS1"),
                pattern("tdx-pattern", "TDX1"),
            ],
            trips,
        )
        .with_routes(
            ["GTFS1", "TDX1"]
                .map(|id| TransitRoute {
                    id: TransitRouteId::new(id),
                    mode: TransitMode::Bus,
                    label: id.into(),
                })
                .to_vec(),
        )
        .with_calendars(vec![every_day])
    }

    fn schedule(sub_route: &str) -> BusSchedule {
        let every_day = serde_json::json!({
            "Monday": 1, "Tuesday": 1, "Wednesday": 1, "Thursday": 1,
            "Friday": 1, "Saturday": 1, "Sunday": 1
        });
        serde_json::from_value(serde_json::json!({
            "SubRouteUID": sub_route,
            "Direction": 0,
            "Frequencys": [{
                "StartTime": "09:00",
                "EndTime": "16:00",
                "MinHeadwayMins": 8,
                "MaxHeadwayMins": 12,
                "ServiceDay": every_day
            }]
        }))
        .unwrap()
    }

    fn headways_of<'a>(headways: &'a [PatternHeadways], pattern: &str) -> &'a [Option<u32>] {
        &headways
            .iter()
            .find(|headways| headways.pattern_id.as_str() == pattern)
            .unwrap()
            .seconds_by_period
    }

    #[test]
    fn tdx_frequencies_become_period_headways_for_patterns_without_gtfs_trips() {
        let (headways, diagnostics) = pattern_headways(
            &network(),
            &[schedule("TDX1"), schedule("UNKNOWN")],
            WEEKDAY,
            WEEKEND,
        );

        let tdx = headways_of(&headways, "tdx-pattern");
        assert_eq!(tdx[ServicePeriod::WeekdayDay.index()], Some(600));
        assert_eq!(tdx[ServicePeriod::Night.index()], None);
        assert_eq!(tdx[ServicePeriod::WeekdayEveningPeak.index()], None);
        assert_eq!(diagnostics.from_tdx, 1);
        assert_eq!(
            diagnostics.unmapped_tdx_sub_routes,
            BTreeSet::from(["UNKNOWN:0".to_owned()])
        );
    }

    #[test]
    fn gtfs_trips_take_precedence_over_tdx_schedules() {
        let (headways, diagnostics) =
            pattern_headways(&network(), &[schedule("GTFS1")], WEEKDAY, WEEKEND);

        assert_eq!(diagnostics.from_gtfs, 1);
        assert_eq!(diagnostics.from_tdx, 0);
        let gtfs = headways_of(&headways, "gtfs-pattern");
        assert_eq!(gtfs[ServicePeriod::WeekdayDay.index()], Some(4200));
    }

    #[test]
    fn only_patterns_without_gtfs_trips_receive_estimate_headways() {
        let network = network();
        let (headways, _) = pattern_headways(&network, &[schedule("TDX1")], WEEKDAY, WEEKEND);
        let binding = |id: &str| BusPatternBinding {
            tdx_service_id: id.into(),
            realtime_scope: "Taipei".into(),
            tdx_route_uid: None,
            headway_seconds_by_period: Vec::new(),
        };
        let mut bindings = TdxProviderBindings {
            bus_patterns: [
                (TransitPatternId::new("gtfs-pattern"), binding("GTFS1")),
                (TransitPatternId::new("tdx-pattern"), binding("TDX1")),
            ]
            .into(),
            ..TdxProviderBindings::default()
        };

        let attached = attach_estimate_headways(&mut bindings, &network, &headways);

        assert_eq!(attached, 1);
        assert!(
            bindings.bus_patterns[&TransitPatternId::new("gtfs-pattern")]
                .headway_seconds_by_period
                .is_empty()
        );
        assert_eq!(
            bindings.bus_patterns[&TransitPatternId::new("tdx-pattern")]
                .headway_seconds_by_period[ServicePeriod::WeekdayDay.index()],
            Some(600)
        );
    }

    #[test]
    fn estimate_service_days_are_the_next_local_wednesday_and_saturday() {
        let (weekday, weekend) =
            estimate_service_days(time::macros::datetime!(2026-11-24 22:30 UTC));
        assert_eq!(weekday, date!(2026 - 11 - 25));
        assert_eq!(weekend, date!(2026 - 11 - 28));
        let (weekday, weekend) =
            estimate_service_days(time::macros::datetime!(2026-11-28 10:00 +08:00));
        assert_eq!(weekday, date!(2026 - 12 - 02));
        assert_eq!(weekend, date!(2026 - 11 - 28));
    }

    #[test]
    fn clock_minutes_reads_hh_mm_and_rejects_garbage() {
        assert_eq!(clock_minutes("09:30:00"), Some(570));
        assert_eq!(clock_minutes("25:10"), Some(1510));
        assert_eq!(clock_minutes("9:75"), None);
        assert_eq!(clock_minutes("noon"), None);
    }
}
