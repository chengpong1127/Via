# TDX static-data contract review (2026-09-16)

## Scope and sources

This is a current-contract review for Via's `taipei-new-taipei` static fetch
scope. The review used the official TDX Basic API service catalog and the
official Swagger/OpenAPI documents returned by that catalog, all retrieved on
2026-09-16 UTC.

- [TDX Basic API service catalog](https://tdx.transportdata.tw/webapi/serviceCategory/ForSwagger)
- [TDX Bus v2 OpenAPI](https://tdx.transportdata.tw/webapi/File/Swagger/V3/2998e851-81d0-40f5-b26d-77e2f5ac4118)
- [TDX Bus v3 OpenAPI](https://tdx.transportdata.tw/webapi/File/Swagger/V3/939d7b26-14d6-40bc-bdc9-a076e3f8d4dc)
- [TDX Rail v2 OpenAPI](https://tdx.transportdata.tw/webapi/File/Swagger/V3/268fc230-2e04-471b-a728-a726167c1cfc)
- [TDX Rail v3 OpenAPI](https://tdx.transportdata.tw/webapi/File/Swagger/V3/5fa88b0c-120b-43f1-b188-c379ddb2593d)
- [TDX Bike v2 OpenAPI](https://tdx.transportdata.tw/webapi/File/Swagger/V3/2cc9b888-a592-496f-99de-9ab35b7fb70d)

The OpenAPI documents declare OpenAPI 3.0.4 and the Basic API server
`https://tdx.transportdata.tw/api/basic`. The relevant GET operations all
declare `$format` as a required query parameter with `JSON` and `XML` as the
allowed values. Via explicitly requests `$format=JSON`. TDX's official
[OData query guide](https://motc-ptx.gitbook.io/tdx-xin-shou-zhi-yin/api-te-se-shuo-ming/zhi-yuan-odata-cha-xun-yu-fa/odata-cha-xun)
says omitting `$top` returns all records, so the fetcher intentionally does
not add a page-size limit.

## Dataset decisions

### Taipei Bus and New Taipei Bus

The current Bus v2 contract contains these operations, with `City` as a
required path parameter and both `Taipei` and `NewTaipei` in the documented
city enum:

- `/v2/Bus/Route/City/{City}`
- `/v2/Bus/Stop/City/{City}`
- `/v2/Bus/StopOfRoute/City/{City}`
- `/v2/Bus/Schedule/City/{City}`

The current Bus v3 document contains the same path families, but its current
`City` enum contains only `Tainan`. It is therefore not a valid replacement
for Via's Taipei/New Taipei sources. The v2 response shapes used by Via remain
compatible: bus topology uses `StopUID`, `Stops`, `StopSequence`, and
`StopPosition`. The Schedule contract supports either fixed-trip
`Timetables[].StopTimes[]` records or headway-based `Frequencys[]` records; the
downloaded snapshot contains both variants across the Taipei and New Taipei
feeds. Service days are represented by structured `ServiceDay` weekday flags.

### YouBike

The current Bike v2 contract contains
`/v2/Bike/Station/City/{City}`. Its `City` path enum includes both `Taipei` and
`NewTaipei`, and its JSON response is an array of stations containing
`StationUID`, `StationName`, and `StationPosition`. No version migration is
needed.

### Taipei Metro / TRTC

The current Rail v2 contract contains all five TRTC sources used by Via:

- `/v2/Rail/Metro/Station/{RailSystem}`
- `/v2/Rail/Metro/StationOfLine/{RailSystem}`
- `/v2/Rail/Metro/StationTimeTable/{RailSystem}`
- `/v2/Rail/Metro/S2STravelTime/{RailSystem}`
- `/v2/Rail/Metro/Frequency/{RailSystem}`

`TRTC` is the documented rail-system value. The response fields consumed by
Via are current: line station lists use `Stations[].StationID` and
`Sequence`; the OAS defines station timetable arrival/departure fields, while
the downloaded snapshot's timetable entries contain `DepartureTime` and
`TrainType`; inter-station timing uses `TravelTimes[].FromStationID`,
`ToStationID`, and `RunTime`; and frequency uses `OperationTime` plus
`Headways[].StartTime`, `EndTime`, and `MinHeadwayMins`.

### TRA

The current Rail v3 contract contains:

- `/v3/Rail/TRA/Station`
- `/v3/Rail/TRA/GeneralTrainTimetable`

The OAS describes wrapper schemas for both operations. The downloaded JSON
snapshot currently returns top-level arrays for both operations: station
records contain `StationUID`, `StationID`, and `StationPosition`; timetable
records contain `GeneralTimetable`, whose value contains `GeneralTrainInfo`,
`StopTimes`, and structured `ServiceDay` weekday flags. Via accepts the
documented wrapper form and the observed array form at the data boundary, and
uses the v3 station IDs consistently for timetable alias resolution.

### THSR and the failing date-specific endpoint

The current Rail v2 document still declares
`/v2/Rail/THSR/DailyTimetable/TrainDate/{TrainDate}`. The path parameter is
required and the documented format is exactly `yyyy-MM-dd`; it is not a query
parameter. The same document also declares the date-specific station, OD,
train-number, and train-date-list variants.

The date-specific endpoint is therefore not replaced by an invented path or a
different date format. A 400 response for a particular date is not evidence in
the contract that the path has been removed. Via's static route-planning source
now uses the separately documented current operation
`/v2/Rail/THSR/GeneralTimetable`, whose records contain regular train stop
times, `GeneralTrainInfo`, `ServiceDay`, and effective/expiring timetable
metadata. THSR remains v2 because the current Rail v3 document does not expose
a THSR equivalent.

The snapshot date `2026-09-15` is still recorded in the raw snapshot manifest,
but it no longer changes the TRA or THSR source URL. `ServiceDay` is
normalized into scheduled-trip weekdays; effective/expiring dates remain
provider metadata in the raw source and are not used as a new routing concept.

## Via changes derived from the contracts

- Add `$format=JSON` to every static request and source identity.
- Move TRA station and timetable ingestion to Rail v3.
- Replace the THSR date-bound static source with Rail v2 GeneralTimetable.
- Accept official TRA `Stations` and `TrainTimetables` envelopes as well as
  the observed top-level arrays, and unwrap THSR `GeneralTimetable` records.
- Read official structured `ServiceDay` weekday flags.
- Treat THSR's nullable first-stop `ArrivalTime` as the departure time when
  constructing the provider-neutral scheduled stop.
- Validate an existing raw file against the previous manifest's endpoint/path
  identity before reusing it. A valid file without a prior manifest remains
  eligible for partial-snapshot recovery.

The provider-specific handling remains in `server/src/data.rs`; routing and the
domain receive only `TransitNetwork`, `MetroTiming`, and `ScheduledTrip`.
