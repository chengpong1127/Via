# THSR realtime timing feasibility

Date: 2026-09-19  
Via revision inspected: `276626286b96cecf5c757e0fea2853401a292392`  
TDX source checked: current v2 public-transport rail OAS and Basic-service catalogue

## Decision

TDX does not currently expose a THSR feed that is suitable for Via departure
timing. THSR remains intentionally static-only:

- do not change routing behavior;
- keep `ThsrDepartureSource` schedule-backed;
- keep static THSR departures as `TimingBasis::Schedule` with
  `TimingReliability::Reliable`;
- do not infer delay or predicted times from alerts, seat availability, or
  timetable publication timestamps.

This is different from Bus, Metro, and TRA because their current TDX contracts
provide timing-oriented realtime fields or can support a clearly defined
effective-time calculation. The THSR contracts checked here do not.

## Current official THSR endpoints

The current TDX v2 rail OAS is served from
`https://tdx.transportdata.tw/api/basic` and lists these THSR families:

### Operational or realtime-related data

| Endpoint family | TDX meaning | Catalogue cadence | Useful for routing timing? |
| --- | --- | --- | --- |
| `/v2/Rail/THSR/AlertInfo` | Realtime service disruption/operating-condition events | Every minute | No |
| `/v2/Rail/THSR/AvailableSeatStatusList` and `/AvailableSeatStatusList/{StationID}` | Station-facing remaining-seat board | Every minute | No |
| `/v2/Rail/THSR/AvailableSeatStatus/Train/Leg/Today` and `/TrainDate/{TrainDate}` | Remaining-seat status by train leg | Every minute in the catalogue | No |
| `/v2/Rail/THSR/AvailableSeatStatus/Train/OD/...` | Remaining-seat status by train and origin/destination | The OAS says day D is every 10 minutes and D+1 through D+27 are daily at 10:00, 16:00, and 22:00 | No |
| `/v2/Rail/THSR/News` | THSR news messages | Unscheduled | No |

The OAS also includes static or timetable data such as:

- `/v2/Rail/THSR/GeneralTimetable` and `/GeneralTimetable/TrainNo/{TrainNo}`;
- `/v2/Rail/THSR/DailyTrainInfo/...`;
- `/v2/Rail/THSR/DailyTimetable/...`;
- station, station-exit, shape, fare, and free-seating-car endpoints.

Those endpoints do not add a THSR actual/predicted timing contract. The Basic
service catalogue labels the THSR general and daily timetable services
`不定時` (unscheduled), while the operational alert and seat services are
labelled `每分鐘` (every minute).

Sources: [TDX current v2 rail OAS](https://tdx.transportdata.tw/webapi/File/Swagger/V3/268fc230-2e04-471b-a728-a726167c1cfc),
[TDX Basic-service catalogue](https://tdx.transportdata.tw/webapi/service?category=basic),
[THSR AlertInfo service metadata](https://tdx.transportdata.tw/webapi/service/GetServiceDetail/76a76db1-d91c-4b4f-a737-8252134793f6),
[THSR seat-board service metadata](https://tdx.transportdata.tw/webapi/service/GetServiceDetail/aaf1ff96-8987-4bfd-a88a-4bbb97ee901b).

## Payload and semantic findings

### AlertInfo

`AlertInfo` contains `AlertID`, title/description, `Status`, `Scope`,
`Direction`, `Level`, effect/reason, alert URL, occurrence/start/end/publish
times, and source/platform update timestamps. `Scope.LineSections` contains a
line ID and optional starting/ending station IDs and names.

The schema contains no `TrainNo`, actual arrival, predicted arrival, actual
departure, predicted departure, delay seconds, or train position. `Status` is
an operating condition (`normal`, another abnormal state, or whole-line
stop), not a per-train delay measurement. It can support a future service
condition or disruption feature, but cannot safely rewrite a scheduled THSR
trip's stop times.

### Available-seat status

The seat-status wrapper contains `UpdateTime`, `SrcUpdateTime`, `Count`,
`TrainDate`, and `AvailableSeats`. Each seat record contains `TrainNo`,
direction, origin/destination station identity, and stop stations. Each stop
station contains standard/business seat states (`O`, `L`, or `X`) and station
identity; it contains no arrival/departure time, delay, or position.

TDX explicitly warns in the wrapper schema that when THSR operating status is
abnormal, remaining-seat data remains at the last normally operating state.
That makes it unsuitable even as an indirect “the train is moving on time”
signal.

### Timetables and daily train information

The general timetable has `TrainNo`, direction, origin/destination, service-day
flags, and `StopTimes`. Each stop time has scheduled `ArrivalTime` and
`DepartureTime` strings in `HH:mm:ss` format. Daily timetable records add
`TrainDate`, `UpdateTime`, and `VersionID`. None of these schemas has an actual
or predicted time, a delay field, or a live-position field. `UpdateTime` and
`SrcUpdateTime` identify publication/refresh state; they do not mean that the
listed times are observations.

The OAS path inventory has no THSR `LiveBoard`, `LivePosition`,
`TrainLiveBoard`, or `LiveTrainDelay` endpoint. The same OAS does define
delay-oriented fields such as `DelayTime` for TRA, so this is an absence in the
current THSR contract rather than a naming ambiguity.

Sources: [THSR AlertInfo operation and response](https://tdx.transportdata.tw/webapi/File/Swagger/V3/268fc230-2e04-471b-a728-a726167c1cfc#/THSR/THSRApi_AlertInfo_2127),
[THSR timetable operations](https://tdx.transportdata.tw/webapi/File/Swagger/V3/268fc230-2e04-471b-a728-a726167c1cfc#/THSR/THSRApi_GeneralTimetable_2122),
[THSR seat-status operations](https://tdx.transportdata.tw/webapi/File/Swagger/V3/268fc230-2e04-471b-a728-a726167c1cfc#/THSR/THSRApi_AvailableSeatStatus_2135),
[THSR OAS schemas](https://tdx.transportdata.tw/webapi/File/Swagger/V3/268fc230-2e04-471b-a728-a726167c1cfc).

## Identity matching to Via

Via currently fetches THSR static data from
`v2/Rail/THSR/Station` and `v2/Rail/THSR/GeneralTimetable` in
[`server/src/data.rs`](../../server/src/data.rs#L2104-L2122). The normalizer:

- treats a `thsr` source as `TransitMode::HighSpeedRail`;
- uses `GeneralTrainInfo.TrainNo` as the train identity;
- constructs the service ID as `thsr:{TrainNo}`;
- uses `thsr_schedule` as the timing source;
- uses `thsr:{TrainNo}:{TrainDate}` for a dated record when `TrainDate` exists,
  otherwise falls back to `thsr:{TrainNo}:{index}`.

Therefore:

- a seat payload's `TrainNo` can identify the logical Via THSR service;
- the seat payload's `TrainDate` can identify a dated trip concept when Via is
  using a dated daily timetable;
- the current static source is the general timetable, whose records do not
  contain `TrainDate`, so the current artifact may use the index-based
  scheduled-trip suffix;
- `AlertInfo` cannot be matched to a Via trip: it has no train number and only
  line/section/direction scope.

Even where identity is sufficient, seat availability is not timing data. No
current THSR endpoint provides the value needed to construct a trustworthy
delay-adjusted `EffectiveTrip`.

## Via design confirmation

[`ThsrDepartureSource`](../../server/src/providers/timing/thsr.rs#L15-L53) has no
TDX client and delegates to `ScheduleDepartureResolver`. Its static reliability
is `Reliable`; the existing contract test asserts `Schedule + Reliable` for
THSR ([`dispatcher.rs`](../../server/tests/timing/dispatcher.rs#L160-L199)).

That behavior is the intended design until TDX publishes a documented THSR
per-train actual/predicted timing feed with a stable train/date identity and
clear freshness semantics. No THSR realtime timing implementation should be
added based on the currently available alert or seat feeds.
