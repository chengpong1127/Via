use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use time::{Date, OffsetDateTime};

use crate::domain::{
    ids::{BoardingPointId, EffectiveTripId, TransitPatternId, TransitTripId},
    realtime::NormalizedRealtimeValue,
    transit::runtime::TransitRuntime,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimingBasis {
    RealtimePrediction,
    DelayAdjustedSchedule,
    Schedule,
    FrequencyEstimate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingReliability {
    Reliable,
    Fallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DepartureQuery {
    pub pattern_id: TransitPatternId,
    pub board_stop_id: BoardingPointId,
    pub board_index: Option<usize>,
    pub after: OffsetDateTime,
    pub before: Option<OffsetDateTime>,
    pub limit: usize,
}

impl DepartureQuery {
    pub fn new(
        pattern_id: TransitPatternId,
        board_stop_id: BoardingPointId,
        after: OffsetDateTime,
        limit: usize,
    ) -> Self {
        Self {
            pattern_id,
            board_stop_id,
            board_index: None,
            after,
            before: None,
            limit,
        }
    }

    pub fn at_position(
        pattern_id: TransitPatternId,
        board_stop_id: BoardingPointId,
        board_index: usize,
        after: OffsetDateTime,
        limit: usize,
    ) -> Self {
        Self {
            pattern_id,
            board_stop_id,
            board_index: Some(board_index),
            after,
            before: None,
            limit,
        }
    }

    pub fn until(mut self, before: OffsetDateTime) -> Self {
        self.before = Some(before);
        self
    }

    pub fn resolve_board_index(&self, transit: &TransitRuntime) -> Result<usize, DepartureError> {
        let pattern = transit
            .pattern(&self.pattern_id)
            .ok_or(DepartureError::InvalidQuery)?;
        match self.board_index {
            Some(index) if pattern.stops.get(index) == Some(&self.board_stop_id) => Ok(index),
            Some(_) => Err(DepartureError::InvalidQuery),
            None => pattern
                .stops
                .iter()
                .position(|stop| stop == &self.board_stop_id)
                .ok_or(DepartureError::InvalidQuery),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TimingSnapshot {
    departures_by_position: HashMap<(TransitPatternId, usize), StoredDepartures>,
    trip_by_key: HashMap<(EffectiveTripId, OffsetDateTime), Arc<EffectiveTrip>>,
    failures: Vec<DepartureError>,
}

#[derive(Clone, Debug, Default)]
pub struct SnapshotDepartures {
    pub preferred_trips: Vec<EffectiveTrip>,
    pub fallback_trips: Vec<EffectiveTrip>,
    pub failure: Option<DepartureError>,
}

#[derive(Clone, Debug, Default)]
struct StoredDepartures {
    preferred_trips: Vec<Arc<EffectiveTrip>>,
    fallback_trips: Vec<Arc<EffectiveTrip>>,
    failure: Option<DepartureError>,
}

impl TimingSnapshot {
    pub(crate) fn trace_summary(&self) -> (usize, usize, usize, Vec<(TimingBasis, usize)>) {
        let mut successful = 0;
        let mut empty = 0;
        let mut failed = 0;
        let mut count_by_basis = HashMap::<TimingBasis, usize>::new();
        for departures in self.departures_by_position.values() {
            if departures.preferred_trips.is_empty() && departures.fallback_trips.is_empty() {
                if departures.failure.is_some() {
                    failed += 1;
                } else {
                    empty += 1;
                }
            } else {
                successful += 1;
                for trip in departures
                    .preferred_trips
                    .iter()
                    .chain(&departures.fallback_trips)
                {
                    *count_by_basis.entry(trip.timing_basis).or_default() += 1;
                }
            }
        }
        let mut basis_counts = count_by_basis.into_iter().collect::<Vec<_>>();
        basis_counts.sort_by_key(|(basis, _)| format!("{basis:?}"));
        (successful, empty, failed, basis_counts)
    }

    pub fn insert(
        &mut self,
        pattern_id: TransitPatternId,
        board_index: usize,
        trips: Vec<EffectiveTrip>,
    ) {
        self.insert_entry(
            pattern_id,
            board_index,
            SnapshotDepartures {
                preferred_trips: trips,
                fallback_trips: Vec::new(),
                failure: None,
            },
        );
    }

    pub fn insert_entry(
        &mut self,
        pattern_id: TransitPatternId,
        board_index: usize,
        mut departures: SnapshotDepartures,
    ) {
        let sort = |trips: &mut Vec<EffectiveTrip>| {
            trips.sort_by(|left, right| {
                left.stop_times
                    .get(board_index)
                    .map(|stop| stop.departure_at)
                    .cmp(
                        &right
                            .stop_times
                            .get(board_index)
                            .map(|stop| stop.departure_at),
                    )
                    .then(left.id.as_str().cmp(right.id.as_str()))
            });
        };
        sort(&mut departures.preferred_trips);
        sort(&mut departures.fallback_trips);
        if let Some(failure) = departures.failure.clone() {
            self.failures.push(failure);
        }
        let mut intern = |trip: EffectiveTrip| {
            let first_departure = trip
                .stop_times
                .first()
                .map(|stop| stop.departure_at)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH);
            self.trip_by_key
                .entry((trip.id.clone(), first_departure))
                .or_insert_with(|| Arc::new(trip))
                .clone()
        };
        let stored = StoredDepartures {
            preferred_trips: departures
                .preferred_trips
                .into_iter()
                .map(&mut intern)
                .collect(),
            fallback_trips: departures.fallback_trips.into_iter().map(intern).collect(),
            failure: departures.failure,
        };
        self.departures_by_position
            .insert((pattern_id, board_index), stored);
    }

    pub fn record_failure(&mut self, failure: DepartureError) {
        self.failures.push(failure);
    }

    pub fn all_positions_failed(&self) -> bool {
        !self.failures.is_empty()
            && self.departures_by_position.values().all(|departures| {
                departures.failure.is_some()
                    && departures.preferred_trips.is_empty()
                    && departures.fallback_trips.is_empty()
            })
    }

    pub fn first_failure(&self) -> Option<&DepartureError> {
        self.failures.first()
    }

    pub fn departures(
        &self,
        pattern_id: &TransitPatternId,
        board_index: usize,
        after: OffsetDateTime,
        limit: usize,
    ) -> Vec<&Arc<EffectiveTrip>> {
        let Some(departures) = self
            .departures_by_position
            .get(&(pattern_id.clone(), board_index))
        else {
            return Vec::new();
        };
        let departs_in_time = |trip: &&Arc<EffectiveTrip>| {
            trip.stop_times
                .get(board_index)
                .is_some_and(|stop| stop.departure_at >= after)
        };
        let mut preferred = departures
            .preferred_trips
            .iter()
            .filter(departs_in_time)
            .collect::<Vec<_>>();
        if preferred.is_empty() {
            return departures
                .fallback_trips
                .iter()
                .filter(departs_in_time)
                .take(limit)
                .collect();
        }
        if departures
            .preferred_trips
            .iter()
            .any(|trip| trip.scheduled_trip_id.is_none())
        {
            preferred.truncate(limit);
            return preferred;
        }
        let observed = departures
            .preferred_trips
            .iter()
            .filter_map(|trip| trip.scheduled_trip_id.as_ref())
            .collect::<HashSet<_>>();
        preferred.extend(
            departures
                .fallback_trips
                .iter()
                .filter(departs_in_time)
                .filter(|trip| {
                    trip.scheduled_trip_id
                        .as_ref()
                        .is_none_or(|id| !observed.contains(id))
                }),
        );
        preferred.sort_by(|left, right| {
            left.stop_times[board_index]
                .departure_at
                .cmp(&right.stop_times[board_index].departure_at)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        preferred.truncate(limit);
        preferred
    }
}

#[async_trait]
pub trait TimingSnapshotLoader: Send + Sync {
    async fn load_snapshot(&self, queries: Vec<DepartureQuery>) -> TimingSnapshot;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveTrip {
    pub id: EffectiveTripId,
    pub pattern_id: TransitPatternId,
    pub scheduled_trip_id: Option<TransitTripId>,
    pub stop_times: Vec<EffectiveStopTime>,
    pub timing_basis: TimingBasis,
    pub timing_reliability: TimingReliability,
    pub observed_at: Option<OffsetDateTime>,
    pub evidence: Vec<NormalizedRealtimeValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveStopTime {
    pub stop_id: BoardingPointId,
    pub arrival_at: OffsetDateTime,
    pub departure_at: OffsetDateTime,
}

impl EffectiveTrip {
    pub fn from_scheduled(
        transit: &TransitRuntime,
        service_day: Date,
        trip_id: &TransitTripId,
    ) -> Option<Self> {
        let trip = transit.scheduled_trip(trip_id)?;
        let pattern = transit.pattern(&trip.pattern_id)?;
        if !transit.calendar(&trip.calendar_id)?.runs_on(service_day)
            || trip.stop_times.len() != pattern.stops.len()
        {
            return None;
        }
        let start = transit.service_day_start(service_day);
        let stop_times = pattern
            .stops
            .iter()
            .zip(&trip.stop_times)
            .map(|(stop_id, stop_time)| EffectiveStopTime {
                stop_id: stop_id.clone(),
                arrival_at: start + time::Duration::seconds(i64::from(stop_time.arrival_seconds)),
                departure_at: start
                    + time::Duration::seconds(i64::from(stop_time.departure_seconds)),
            })
            .collect();
        Some(Self {
            id: EffectiveTripId::new(trip.id.as_str()),
            pattern_id: trip.pattern_id.clone(),
            scheduled_trip_id: Some(trip.id.clone()),
            stop_times,
            timing_basis: TimingBasis::Schedule,
            timing_reliability: TimingReliability::Reliable,
            observed_at: None,
            evidence: Vec::new(),
        })
    }

    pub fn from_frequency(
        transit: &TransitRuntime,
        service_day: Date,
        frequency: &super::network::TransitFrequency,
        first_departure_seconds: u32,
    ) -> Option<Self> {
        if !transit
            .calendar(&frequency.calendar_id)?
            .runs_on(service_day)
        {
            return None;
        }
        let pattern = transit.pattern(&frequency.pattern_id)?;
        if frequency.template_stop_times.len() != pattern.stops.len() {
            return None;
        }
        let template_start = frequency.template_stop_times.first()?.departure_seconds;
        let service_day_start = transit.service_day_start(service_day);
        let stop_times = pattern
            .stops
            .iter()
            .zip(&frequency.template_stop_times)
            .map(|(stop_id, template)| {
                let arrival_offset =
                    i64::from(template.arrival_seconds) - i64::from(template_start);
                let departure_offset =
                    i64::from(template.departure_seconds) - i64::from(template_start);
                EffectiveStopTime {
                    stop_id: stop_id.clone(),
                    arrival_at: service_day_start
                        + time::Duration::seconds(
                            i64::from(first_departure_seconds) + arrival_offset,
                        ),
                    departure_at: service_day_start
                        + time::Duration::seconds(
                            i64::from(first_departure_seconds) + departure_offset,
                        ),
                }
            })
            .collect::<Vec<_>>();
        if stop_times.windows(2).any(|times| {
            times[0].arrival_at > times[0].departure_at
                || times[0].departure_at > times[1].arrival_at
        }) {
            return None;
        }
        let timing_basis = if frequency.exact_times {
            TimingBasis::Schedule
        } else {
            TimingBasis::FrequencyEstimate
        };
        Some(Self {
            id: EffectiveTripId::new(format!(
                "frequency:{}:{}:{}",
                frequency.id.as_str(),
                service_day,
                first_departure_seconds
            )),
            pattern_id: frequency.pattern_id.clone(),
            scheduled_trip_id: None,
            stop_times,
            timing_basis,
            timing_reliability: TimingReliability::Fallback,
            observed_at: None,
            evidence: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum DepartureError {
    #[error("departure resolver timed out")]
    TimedOut,
    #[error("departure resolver was rate limited")]
    RateLimited,
    #[error("departure resolver is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("departure resolver returned an invalid response")]
    InvalidResponse,
    #[error("departure resolver configuration is invalid")]
    InvalidConfiguration,
    #[error("departure query references an invalid pattern or boarding point")]
    InvalidQuery,
}

impl DepartureError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::TimedOut | Self::RateLimited | Self::TemporarilyUnavailable
        )
    }
}

#[derive(Clone)]
pub struct ScheduleTiming {
    transit: Arc<TransitRuntime>,
}

impl ScheduleTiming {
    pub fn new(transit: Arc<TransitRuntime>) -> Self {
        Self { transit }
    }

    pub fn transit(&self) -> &Arc<TransitRuntime> {
        &self.transit
    }
    pub async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let Some(pattern) = self.transit.pattern(&query.pattern_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if !pattern.stops.contains(&query.board_stop_id) {
            return Err(DepartureError::InvalidQuery);
        }
        let board_index = query.resolve_board_index(&self.transit)?;
        let first_service_day = self.transit.service_day(query.after);
        let last_service_day = query
            .before
            .map(|before| self.transit.service_day(before))
            .unwrap_or(first_service_day);
        let mut service_day = first_service_day;
        let mut departures = Vec::new();
        loop {
            departures.extend(
                self.transit
                    .scheduled_trips_boarding_at(&query.pattern_id, board_index)
                    .filter_map(|trip| {
                        self.transit
                            .calendar(&trip.calendar_id)?
                            .runs_on(service_day)
                            .then(|| {
                                EffectiveTrip::from_scheduled(&self.transit, service_day, &trip.id)
                            })
                            .flatten()
                    }),
            );
            if service_day >= last_service_day {
                break;
            }
            let Some(next_day) = service_day.next_day() else {
                break;
            };
            service_day = next_day;
        }
        departures.retain(|trip| {
            let departure_at = trip.stop_times[board_index].departure_at;
            departure_at >= query.after && query.before.is_none_or(|before| departure_at <= before)
        });
        departures.sort_by(|left, right| {
            left.stop_times[board_index]
                .departure_at
                .cmp(&right.stop_times[board_index].departure_at)
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        departures.truncate(query.limit);
        Ok(departures)
    }

    pub fn next_frequency_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let Some(pattern) = self.transit.pattern(&query.pattern_id) else {
            return Err(DepartureError::InvalidQuery);
        };
        if !pattern.stops.contains(&query.board_stop_id) {
            return Err(DepartureError::InvalidQuery);
        }
        let board_index = query.resolve_board_index(&self.transit)?;
        let first_service_day = self.transit.service_day(query.after);
        let last_service_day = query
            .before
            .map(|before| self.transit.service_day(before))
            .unwrap_or(first_service_day);
        let mut departures = Vec::new();
        let mut service_day = first_service_day;
        loop {
            let day_start = self.transit.service_day_start(service_day);
            let after_seconds =
                u32::try_from((query.after - day_start).whole_seconds().max(0)).unwrap_or(u32::MAX);
            let before_seconds = query
                .before
                .map(|before| {
                    u32::try_from((before - day_start).whole_seconds().max(0)).unwrap_or(u32::MAX)
                })
                .unwrap_or(u32::MAX);
            for frequency in self.transit.frequencies_of(&query.pattern_id) {
                if !self
                    .transit
                    .calendar(&frequency.calendar_id)
                    .is_some_and(|calendar| calendar.runs_on(service_day))
                    || frequency.template_stop_times.len() != pattern.stops.len()
                {
                    continue;
                }
                let Some(template_start) = frequency
                    .template_stop_times
                    .first()
                    .map(|stop_time| stop_time.departure_seconds)
                else {
                    continue;
                };
                let Some(board_offset) = frequency.template_stop_times[board_index]
                    .departure_seconds
                    .checked_sub(template_start)
                else {
                    continue;
                };
                let Some(start_seconds) = frequency.start_seconds.checked_add(board_offset) else {
                    continue;
                };
                let Some(end_seconds) = frequency.end_seconds.checked_add(board_offset) else {
                    continue;
                };
                for board_seconds in expand_frequency_departures(
                    start_seconds,
                    end_seconds,
                    frequency.headway_seconds,
                    after_seconds,
                    before_seconds,
                    query.limit,
                ) {
                    let Some(first_departure_seconds) = board_seconds.checked_sub(board_offset)
                    else {
                        continue;
                    };
                    if let Some(trip) = EffectiveTrip::from_frequency(
                        &self.transit,
                        service_day,
                        frequency,
                        first_departure_seconds,
                    ) {
                        let board_departure = trip.stop_times[board_index].departure_at;
                        if board_departure >= query.after
                            && query.before.is_none_or(|before| board_departure <= before)
                        {
                            departures.push(trip);
                        }
                    }
                }
            }
            if service_day >= last_service_day {
                break;
            }
            let Some(next_day) = service_day.next_day() else {
                break;
            };
            service_day = next_day;
        }
        departures.sort_by(|left, right| {
            left.stop_times[board_index]
                .departure_at
                .cmp(&right.stop_times[board_index].departure_at)
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        departures.dedup_by(|left, right| {
            left.stop_times[board_index].departure_at == right.stop_times[board_index].departure_at
        });
        departures.truncate(query.limit);
        Ok(departures)
    }
}

fn expand_frequency_departures(
    start: u32,
    end: u32,
    headway: u32,
    after: u32,
    before: u32,
    limit: usize,
) -> Vec<u32> {
    if headway == 0 || start > end || limit == 0 {
        return Vec::new();
    }
    let elapsed = after.saturating_sub(start);
    let steps = elapsed.saturating_add(headway - 1) / headway;
    let mut current = start.saturating_add(steps.saturating_mul(headway));
    let mut result = Vec::new();
    while current <= end && current <= before && result.len() < limit {
        result.push(current);
        let Some(next) = current.checked_add(headway) else {
            break;
        };
        current = next;
    }
    result
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::{
        domain::location::GeoCoordinate,
        domain::transit::network::{
            ScheduledStopTime, TransitMode, TransitNetwork, TransitPattern, TransitStop,
            TransitTrip,
        },
        domain::transit::runtime::TransitRuntime,
    };

    fn trip(
        id: &str,
        pattern_id: &TransitPatternId,
        departure_at: OffsetDateTime,
    ) -> EffectiveTrip {
        EffectiveTrip {
            id: EffectiveTripId::new(id),
            pattern_id: pattern_id.clone(),
            scheduled_trip_id: None,
            stop_times: vec![EffectiveStopTime {
                stop_id: BoardingPointId::new("stop"),
                arrival_at: departure_at,
                departure_at,
            }],
            timing_basis: TimingBasis::Schedule,
            timing_reliability: TimingReliability::Fallback,
            observed_at: None,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn snapshot_uses_static_fallback_after_realtime_observations_are_exhausted() {
        let pattern_id = TransitPatternId::new("service");
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert_entry(
            pattern_id.clone(),
            0,
            SnapshotDepartures {
                preferred_trips: vec![trip(
                    "realtime",
                    &pattern_id,
                    datetime!(2026-08-31 10:01 UTC),
                )],
                fallback_trips: vec![trip(
                    "schedule",
                    &pattern_id,
                    datetime!(2026-08-31 11:00 UTC),
                )],
                failure: None,
            },
        );

        let departures = snapshot.departures(&pattern_id, 0, datetime!(2026-08-31 10:30 UTC), 3);
        assert_eq!(departures.len(), 1);
        assert_eq!(departures[0].id, EffectiveTripId::new("schedule"));
    }

    #[test]
    fn unobserved_scheduled_trains_stay_beside_schedule_linked_observations() {
        let pattern_id = TransitPatternId::new("service");
        let linked = |id: &str, scheduled: &str, departure_at| EffectiveTrip {
            scheduled_trip_id: Some(TransitTripId::new(scheduled)),
            ..trip(id, &pattern_id, departure_at)
        };
        let mut snapshot = TimingSnapshot::default();
        snapshot.insert_entry(
            pattern_id.clone(),
            0,
            SnapshotDepartures {
                preferred_trips: vec![linked(
                    "observed-late",
                    "train-2125",
                    datetime!(2026-08-31 21:27 UTC),
                )],
                fallback_trips: vec![
                    linked(
                        "scheduled-early",
                        "train-2055",
                        datetime!(2026-08-31 20:55 UTC),
                    ),
                    linked(
                        "scheduled-late",
                        "train-2125",
                        datetime!(2026-08-31 21:25 UTC),
                    ),
                ],
                failure: None,
            },
        );

        let departures = snapshot.departures(&pattern_id, 0, datetime!(2026-08-31 20:41 UTC), 3);

        assert_eq!(
            departures
                .iter()
                .map(|trip| trip.id.as_str())
                .collect::<Vec<_>>(),
            vec!["scheduled-early", "observed-late"]
        );
    }

    #[tokio::test]
    async fn schedule_snapshot_window_can_cross_the_service_day_boundary() {
        let pattern_id = TransitPatternId::new("service");
        let stop = |id: &str| {
            TransitStop::new(
                BoardingPointId::new(id),
                id,
                GeoCoordinate {
                    latitude: 25.0,
                    longitude: 121.5,
                },
            )
        };
        let trip = |id: &str, weekday, departure_seconds| {
            TransitTrip::fixture_weekdays(
                TransitTripId::new(id),
                pattern_id.clone(),
                vec![weekday],
                vec![
                    ScheduledStopTime::new(departure_seconds, departure_seconds),
                    ScheduledStopTime::new(departure_seconds + 300, departure_seconds + 300),
                ],
            )
        };
        let network = Arc::new(TransitNetwork::fixture(
            time::macros::time!(4:00),
            vec![stop("a"), stop("b")],
            vec![TransitPattern::fixture(
                pattern_id.clone(),
                TransitMode::Bus,
                "service",
                "b",
                vec![BoardingPointId::new("a"), BoardingPointId::new("b")],
            )],
            vec![
                trip("sunday-late", time::Weekday::Sunday, 27 * 3600 + 55 * 60),
                trip("monday-early", time::Weekday::Monday, 4 * 3600 + 10 * 60),
            ],
        ));
        let timing = ScheduleTiming::new(Arc::new(TransitRuntime::new(network)));
        let departures = timing
            .next_departures(
                DepartureQuery::at_position(
                    pattern_id,
                    BoardingPointId::new("a"),
                    0,
                    datetime!(2026-08-31 03:50 +08:00),
                    usize::MAX,
                )
                .until(datetime!(2026-08-31 04:30 +08:00)),
            )
            .await
            .unwrap();

        assert_eq!(departures.len(), 2);
        assert_eq!(departures[0].id, EffectiveTripId::new("sunday-late"));
        assert_eq!(departures[1].id, EffectiveTripId::new("monday-early"));
    }
}
