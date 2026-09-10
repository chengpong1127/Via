//! Immutable timetable data and the small transit portion of Via's planner.
//!
//! This is intentionally independent of TDX (or any other provider). Feed adapters
//! build these records during a static-network refresh, and a planning request only
//! reads the pinned revision. The search is a bounded RAPTOR-style round scan: each
//! round adds at most one transit boarding and retains the parent ride chain.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time, Weekday};

use crate::{
    api::{RequestCoordinate, SelectedMode, normalize_to_taipei, taipei_offset},
    normalization::{BoardingPointId, ScheduledRunId, TransitServiceId, YouBikeStationId},
};

pub const DEFAULT_MAX_TRANSFERS: usize = 2;

pub fn default_service_day_boundary() -> Time {
    Time::from_hms(4, 0, 0).expect("service-day boundary is valid")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitMode {
    Bus,
    Metro,
}

impl TransitMode {
    fn is_selected(self, selected_modes: &[SelectedMode]) -> bool {
        selected_modes.iter().any(|mode| {
            matches!(
                (self, mode),
                (Self::Bus, SelectedMode::Bus) | (Self::Metro, SelectedMode::Metro)
            )
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitStop {
    pub id: BoardingPointId,
    pub name: String,
    pub coordinate: RequestCoordinate,
}

impl TransitStop {
    pub fn new(
        id: BoardingPointId,
        name: impl Into<String>,
        coordinate: RequestCoordinate,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            coordinate,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitService {
    pub id: TransitServiceId,
    pub mode: TransitMode,
    pub label: String,
    pub headsign: String,
    pub pattern: Vec<BoardingPointId>,
    /// True when this operator/capability must provide a fresh Service Condition
    /// before a Journey Option can be displayed.
    pub service_condition_required: bool,
}

impl TransitService {
    pub fn new(
        id: TransitServiceId,
        mode: TransitMode,
        label: impl Into<String>,
        headsign: impl Into<String>,
        pattern: Vec<BoardingPointId>,
    ) -> Self {
        Self {
            id,
            mode,
            label: label.into(),
            headsign: headsign.into(),
            pattern,
            service_condition_required: false,
        }
    }

    pub fn requiring_service_conditions(mut self) -> Self {
        self.service_condition_required = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledStopTime {
    /// Seconds from the beginning of the Service Day. Values may exceed 24 hours.
    pub arrival_seconds: u32,
    /// Seconds from the beginning of the Service Day. Values may exceed 24 hours.
    pub departure_seconds: u32,
}

impl ScheduledStopTime {
    pub fn new(arrival_seconds: u32, departure_seconds: u32) -> Self {
        Self {
            arrival_seconds,
            departure_seconds,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTrip {
    pub id: ScheduledRunId,
    pub service_id: TransitServiceId,
    pub active_weekdays: Vec<Weekday>,
    pub stop_times: Vec<ScheduledStopTime>,
}

impl ScheduledTrip {
    pub fn weekdays(
        id: ScheduledRunId,
        service_id: TransitServiceId,
        active_weekdays: Vec<Weekday>,
        stop_times: Vec<ScheduledStopTime>,
    ) -> Self {
        Self {
            id,
            service_id,
            active_weekdays,
            stop_times,
        }
    }

    fn runs_on(&self, service_day: Date) -> bool {
        self.active_weekdays.contains(&service_day.weekday())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YouBikeStation {
    pub id: YouBikeStationId,
    pub name: String,
    pub coordinate: RequestCoordinate,
}

impl YouBikeStation {
    pub fn new(
        id: YouBikeStationId,
        name: impl Into<String>,
        coordinate: RequestCoordinate,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            coordinate,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitNetwork {
    pub service_day_boundary: Time,
    pub stops: Vec<TransitStop>,
    pub services: Vec<TransitService>,
    pub trips: Vec<ScheduledTrip>,
    pub youbike_stations: Vec<YouBikeStation>,
    pub default_transfer_seconds: u32,
}

impl Default for TransitNetwork {
    fn default() -> Self {
        Self::new(
            default_service_day_boundary(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }
}

impl TransitNetwork {
    pub fn new(
        service_day_boundary: Time,
        stops: Vec<TransitStop>,
        services: Vec<TransitService>,
        trips: Vec<ScheduledTrip>,
        youbike_stations: Vec<YouBikeStation>,
    ) -> Self {
        Self {
            service_day_boundary,
            stops,
            services,
            trips,
            youbike_stations,
            default_transfer_seconds: 120,
        }
    }

    pub fn with_default_transfer_seconds(mut self, seconds: u32) -> Self {
        self.default_transfer_seconds = seconds;
        self
    }

    pub fn stop(&self, id: &BoardingPointId) -> Option<&TransitStop> {
        self.stops.iter().find(|stop| &stop.id == id)
    }

    pub fn service(&self, id: &TransitServiceId) -> Option<&TransitService> {
        self.services.iter().find(|service| &service.id == id)
    }

    pub fn service_day(&self, instant: OffsetDateTime) -> Date {
        let local = normalize_to_taipei(instant);
        let local = local.date().with_time(local.time());
        if local.time() < self.service_day_boundary {
            local.date().previous_day().unwrap_or(local.date())
        } else {
            local.date()
        }
    }

    pub fn service_day_start(&self, service_day: Date) -> OffsetDateTime {
        PrimitiveDateTime::new(service_day, Time::MIDNIGHT).assume_offset(taipei_offset())
    }

    /// Generate timetable parent chains for the selected Bus/Metro modes.
    ///
    /// `access` contains the time at which a rider can reach each Boarding Point.
    /// The caller supplies that access through the street-routing boundary, keeping
    /// Walking and bicycle paths out of the timetable kernel.
    pub fn itineraries(
        &self,
        access: &[TransitAccess],
        query: TransitQuery,
        selected_modes: &[SelectedMode],
    ) -> Vec<TransitItinerary> {
        if access.is_empty() {
            return Vec::new();
        }
        let service_day = match query {
            TransitQuery::DepartAfter { earliest_departure } => {
                self.service_day(earliest_departure)
            }
            TransitQuery::ArriveBy { latest_arrival } => self.service_day(latest_arrival),
        };
        let active_trips = self
            .trips
            .iter()
            .filter(|trip| trip.runs_on(service_day))
            .filter_map(|trip| {
                let service = self.service(&trip.service_id)?;
                service
                    .mode
                    .is_selected(selected_modes)
                    .then_some((trip, service))
            })
            .collect::<Vec<_>>();

        let mut frontier = access
            .iter()
            .map(|access| TransitLabel {
                stop_id: access.stop_id.clone(),
                available_at: access.available_at,
                access_departure_at: access.departure_at,
                rides: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut results = Vec::new();

        for _round in 0..=DEFAULT_MAX_TRANSFERS {
            let mut next = Vec::new();
            for (trip, service) in &active_trips {
                if trip.stop_times.len() != service.pattern.len() {
                    continue;
                }
                for label in &frontier {
                    for board_index in 0..service.pattern.len() {
                        if service.pattern[board_index] != label.stop_id
                            || trip.stop_times[board_index].departure_seconds
                                < seconds_since_service_day(
                                    self.service_day_start(service_day),
                                    label.available_at,
                                )
                        {
                            continue;
                        }
                        for alight_index in (board_index + 1)..service.pattern.len() {
                            let departure = self.service_day_start(service_day)
                                + time::Duration::seconds(i64::from(
                                    trip.stop_times[board_index].departure_seconds,
                                ));
                            let arrival = self.service_day_start(service_day)
                                + time::Duration::seconds(i64::from(
                                    trip.stop_times[alight_index].arrival_seconds,
                                ));
                            let ride = TransitRide {
                                mode: service.mode,
                                service_id: service.id.clone(),
                                trip_id: trip.id.clone(),
                                board_stop_id: service.pattern[board_index].clone(),
                                alight_stop_id: service.pattern[alight_index].clone(),
                                scheduled_departure: departure,
                                scheduled_arrival: arrival,
                            };
                            let mut rides = label.rides.clone();
                            rides.push(ride);
                            let candidate = TransitLabel {
                                stop_id: service.pattern[alight_index].clone(),
                                available_at: arrival,
                                access_departure_at: label.access_departure_at,
                                rides,
                            };
                            if candidate.rides.len() <= DEFAULT_MAX_TRANSFERS + 1 {
                                next.push(candidate);
                            }
                        }
                        break;
                    }
                }
            }
            next.sort_by(|left, right| {
                left.available_at
                    .cmp(&right.available_at)
                    .then(left.rides.len().cmp(&right.rides.len()))
                    .then(ride_signature(&left.rides).cmp(&ride_signature(&right.rides)))
            });
            next.dedup_by(|left, right| {
                left.stop_id == right.stop_id
                    && left.available_at == right.available_at
                    && left.rides == right.rides
            });
            next.truncate(128);

            for label in &next {
                let arrival_allowed = match query {
                    TransitQuery::DepartAfter { .. } => true,
                    TransitQuery::ArriveBy { latest_arrival } => {
                        label.available_at <= latest_arrival
                    }
                };
                if arrival_allowed {
                    results.push(TransitItinerary {
                        service_day,
                        access_departure_at: label.access_departure_at,
                        arrival_at: label.available_at,
                        rides: label.rides.clone(),
                    });
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        results.sort_by(|left, right| {
            left.arrival_at
                .cmp(&right.arrival_at)
                .then(left.transfers().cmp(&right.transfers()))
                .then(ride_signature(&left.rides).cmp(&ride_signature(&right.rides)))
        });
        results.dedup_by(|left, right| {
            left.rides == right.rides && left.access_departure_at == right.access_departure_at
        });
        results.truncate(64);
        results
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TransitLabel {
    stop_id: BoardingPointId,
    available_at: OffsetDateTime,
    access_departure_at: OffsetDateTime,
    rides: Vec<TransitRide>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitAccess {
    pub stop_id: BoardingPointId,
    pub departure_at: OffsetDateTime,
    pub available_at: OffsetDateTime,
}

impl TransitAccess {
    pub fn new(
        stop_id: BoardingPointId,
        departure_at: OffsetDateTime,
        available_at: OffsetDateTime,
    ) -> Self {
        Self {
            stop_id,
            departure_at,
            available_at,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitQuery {
    DepartAfter { earliest_departure: OffsetDateTime },
    ArriveBy { latest_arrival: OffsetDateTime },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitRide {
    pub mode: TransitMode,
    pub service_id: TransitServiceId,
    pub trip_id: ScheduledRunId,
    pub board_stop_id: BoardingPointId,
    pub alight_stop_id: BoardingPointId,
    pub scheduled_departure: OffsetDateTime,
    pub scheduled_arrival: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitItinerary {
    pub service_day: Date,
    pub access_departure_at: OffsetDateTime,
    pub arrival_at: OffsetDateTime,
    pub rides: Vec<TransitRide>,
}

impl TransitItinerary {
    pub fn transfers(&self) -> usize {
        self.rides.len().saturating_sub(1)
    }
}

fn seconds_since_service_day(start: OffsetDateTime, instant: OffsetDateTime) -> u32 {
    let seconds = (normalize_to_taipei(instant) - start).whole_seconds();
    u32::try_from(seconds.max(0)).unwrap_or(u32::MAX)
}

fn ride_signature(rides: &[TransitRide]) -> String {
    rides
        .iter()
        .map(|ride| format!("{}:{}", ride.service_id.as_str(), ride.trip_id.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}
