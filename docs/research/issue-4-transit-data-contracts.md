# Issue #4 research: authoritative Taipei–New Taipei transit data contracts

Status: Historical Research

Research date: 2026-08-29  
Scope: Bus, Metro, and YouBike data needed to construct Journey Options and decide whether a Journey Option qualifies as a Reliable Arrival. Walking is out of scope except where a published transfer duration or station location supports a Walking connector.

## Decision

Use **TDX v2 as the canonical ingestion boundary** for Taipei and New Taipei Bus, Metro, and YouBike data. It is the only authoritative source reviewed that normalizes both cities and all three Travel Modes behind versioned OpenAPI contracts. Keep the Taipei and New Taipei government feeds as diagnostic/fallback adapters, not as a second canonical model.

This decision has two hard limits:

1. TDX is an exchange layer, not a reliability guarantee. Its catalog marks the Bus N1 and Bike availability datasets as **not quality checked**, its open-data licence disclaims warranties and permits provision to stop, and no official end-to-end latency or uptime SLA was found.
2. Taipei Metro cannot supply forward-looking Live Status through TDX. TDX officially confirmed that `LiveBoard/TRTC` emits Taipei Metro records only when a train is at the station (`EstimateTime = 0`). New Taipei Metro systems are not accepted by the TDX `LiveBoard` or `Alert` endpoints at all. Therefore Metro must be modeled from timetable/headway and static travel/transfer time, with operational alerts only where the endpoint actually covers the operator. It must not be presented as live-predicted arrival.

Consequently, Reliable Arrival should be a product-owned calculation over time-stamped source observations and safety buffers. It must degrade explicitly to schedule-only or unavailable when a critical Leg lacks fresh, semantically usable Live Status.

## Evidence status legend

- **Verified**: stated in an official API schema, government catalog, agency document, licence, or official operator/TDX response.
- **Inference**: a conclusion derived from verified facts, but not promised by the source.
- **Unknown**: not established in the reviewed authoritative material.
- **Recommended contract**: Via behavior proposed by this research; it is not an upstream promise.

## Coverage matrix

| Travel Mode | Canonical TDX coverage for Taipei and New Taipei | Live Status usable for Reliable Arrival | Static/planning coverage | Historical observations |
|---|---|---|---|---|
| Bus | `City/Taipei` and `City/NewTaipei` are enumerated by Route, Stop, StopOfRoute, A1 vehicle position, A2 near-stop, N1 estimated arrival, timetable, and Alert services. | **Yes, conditionally.** N1 supplies stop/route/subroute identifiers, direction, status, seconds-to-arrival, next-bus time, timestamps; A2 can corroborate stop progress. | Route, subroute, stop, stop sequence, schedule/headway, shape, operator and alert data. | **Verified at service-family level:** TDX Bus historical service is available from May 2021 onward (excluding today). Exact Taipei/New Taipei N1/A1/A2 date continuity remains **unknown until authenticated sampling**. |
| YouBike | Station and Availability endpoints enumerate both `Taipei` and `NewTaipei`; TDX describes availability, service state, general/e-bike rental count and return slots. | **Yes, conditionally.** Availability is a current count, not a reservation or forecast. | Station UID/ID, name, WGS84 position, address, capacity and service type. | **Verified at catalog level:** TDX historical catalog contains public-bike station versions and `即時剩餘位` records, including Taipei/New Taipei domains. Exact cadence, gaps and date range per city are **unknown until authenticated sampling**. |
| Taipei Metro (`TRTC`) | Lines, stations, station order, transfers, exits/facilities, travel times, first/last train, frequency, station timetable, LiveBoard and Alert. | **No predictive train arrival.** `LiveBoard/TRTC` is only useful at `EstimateTime=0`; Alert is usable as disruption state. | Strong coverage. Station timetables exclude Wenhu Line; TDX directs clients to Frequency for it. Official Taipei Metro also says Wenhu has no fixed timetable because trains are frequent and can be added dynamically. | TDX historical catalog includes Metro versions and real-time-arrival categories. Whether TRTC LiveBoard history is useful is **effectively no** for prediction because the live source itself lacks forward estimates. Alert-history continuity is **unknown**. |
| New Taipei Metro (`NTMC`, plus schema codes `NTDLRT` and `NTALRT`) | Static line/station/timetable coverage is enumerated; Frequency and first/last train include `NTMC`; station timetable enumerates `NTMC`, `NTDLRT`, and `NTALRT`. | **No through the reviewed TDX v2 contract.** `LiveBoard` enumerates only TRTC/KRTC/TYMC/KLRT; `Alert` enumerates TYMC/KRTC/KLRT/TRTC/TRTCMG/TMRT. | Usable static data, but operator-code overlap/migration among `NTMC`, `NTDLRT`, `NTALRT` must be profiled rather than guessed. | The historical catalog contains Metro version services, but exact New Taipei operator coverage is **unknown**. No verified Live Status contract for Reliable Arrival. |

Primary sources: [TDX Bus v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2998e851-81d0-40f5-b26d-77e2f5ac4118), [TDX Rail v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/268fc230-2e04-471b-a728-a726167c1cfc), [TDX Bike v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2cc9b888-a592-496f-99de-9ab35b7fb70d), [TDX historical-service catalog](https://tdx.transportdata.tw/data-service/historical), [TDX administrator response on Taipei Metro LiveBoard](https://tdx.transportdata.tw/topic/detail/d2f5e1d0-ef2f-4a6d-a4ab-f6293c959138), [official Taipei Metro timetable page](https://web.metro.taipei/pages/tw/timetables/013).

## Contract 1: Bus

### Authoritative endpoints and joins

**Verified.** Use the city-qualified TDX v2 endpoints for both `Taipei` and `NewTaipei`:

- `/v2/Bus/Route/City/{City}`: route identity and metadata.
- `/v2/Bus/Stop/City/{City}` and `/Station/City/{City}`: stop-pole and grouped physical-station records.
- `/v2/Bus/StopOfRoute/City/{City}`: ordered stops by subroute and direction.
- `/v2/Bus/EstimatedTimeOfArrival/City/{City}`: N1 arrival estimates.
- `/v2/Bus/RealTimeNearStop/City/{City}`: A2 vehicle/near-stop observations.
- `/v2/Bus/RealTimeByFrequency/City/{City}`: A1 position observations when required.
- `/v2/Bus/Alert/City/{City}` and timetable/headway endpoints: disruption and scheduled fallback.

The N1 schema carries `PlateNumb`, `StopUID`, `RouteUID`, `SubRouteUID`, `Direction`, `EstimateTime` in seconds, `ScheduledTime`, `StopCountDown`, `CurrentStop`, `DestinationStop`, `StopSequence`, `StopStatus`, `NextBusTime`, `IsLastBus`, source timestamps, and TDX `UpdateTime`. `StopStatus` is: 0 normal, 1 not departed, 2 skipped due to traffic control, 3 last bus passed, 4 not operating today. `EstimateTime` may legitimately be null outside normal status; in some cities a not-yet-departed record can still contain an estimate. [TDX Bus v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2998e851-81d0-40f5-b26d-77e2f5ac4118)

**Verified quality limit.** The TDX streaming N1 description says the source recalculates/publishes N1 only when a vehicle on the route departs a stop, so consumers must decrement the received estimate themselves using the source transmission time. This is not a continuously recomputed countdown. The batch endpoint is the right initial contract for Taipei/New Taipei; the streaming endpoint's city enumeration does not include them. [TDX Bus v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2998e851-81d0-40f5-b26d-77e2f5ac4118)

**Verified update declaration.** The government catalog declares the batch N1 dataset updated every minute, free, API-key required, and `dataQuality: 無(未做品檢)` (no quality inspection). `Taipei` and `NewTaipei` are valid path values. [Government catalog: City Bus N1](https://data.gov.tw/dataset/161159)

**Verified direct-source fallback.** New Taipei's own arrival dataset exposes `routeid`, `stopid`, `estimatetime`, and `goback`, declares a two-minute update frequency, and uses Open Government Data License 1.0. [New Taipei bus estimated arrival dataset](https://data.gov.tw/dataset/122907) Taipei Public Transportation Office publishes gzip files for vehicle positions, stop events, and estimated arrivals. Its official v6.3 document defines the local arrival fields and sentinel values (`-1` not departed, `-2` skipped, `-3` last bus passed, `-4` not operating), but these local identifiers are a separate contract. [Taipei Data.Taipei bus API document](https://pto.gov.taipei/News_Content.aspx?n=A1DF07A86105B6BB&sms=2479B630A6BD8079&s=55E8ADD164E4F579)

### Identifier contract

**Verified.** TDX defines `RouteUID = AuthorityCode + RouteID`, `SubRouteUID = AuthorityCode + SubRouteID`, and `StopUID = AuthorityCode + StopID`; the unqualified IDs are source-local internal codes. Direction is a separate value and is not encoded safely by a display route name. [TDX Bus v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2998e851-81d0-40f5-b26d-77e2f5ac4118)

**Recommended contract.** Persist:

```text
bus_leg_key = (SubRouteUID, Direction, boarding_StopUID, alighting_StopUID)
bus_observation_key = (SubRouteUID, Direction, StopUID, PlateNumb?, observed_at)
```

Never join on route name or stop name. Preserve raw `RouteID`, `SubRouteID`, and `StopID` only for trace/debug. Treat `StationID`/`StationGroupID` as grouping hints for Walking connectors; do not merge distinct `StopUID`s merely because names or coordinates are close.

### Reliable Arrival use

**Recommended contract.** A Bus Leg can support Reliable Arrival only when:

- the matching N1 record has `StopStatus = 0` and a non-null `EstimateTime` or `NextBusTime`;
- source age is within the configured freshness budget;
- its `SubRouteUID`, `Direction`, and `StopUID` match the selected Leg;
- the remaining estimate plus boarding and Transfer buffers keeps the Journey Option feasible; and
- a route/stop alert does not invalidate the Leg.

N1 with `StopStatus` 1–4, missing identifiers, stale timestamps, or a schedule-only fallback may still produce a Journey Option, but not a Live Status-backed Reliable Arrival. `PlateNumb = -1` and null estimates are unusable for reliability. A2 is corroborating evidence, not a substitute ETA unless Via owns and validates its own prediction model.

## Contract 2: YouBike

### Authoritative endpoint and schema

**Verified.** Use:

- `/v2/Bike/Station/City/{City}` for station identity, position, address, capacity and `ServiceType`.
- `/v2/Bike/Availability/City/{City}` for `ServiceStatus`, `AvailableRentBikes`, `AvailableReturnBikes`, optional general/e-bike detail, `SrcUpdateTime`, and `UpdateTime`.

Both endpoints enumerate `Taipei` and `NewTaipei`. `StationUID` is defined as `AuthorityCode + StationID`. `ServiceStatus` is 0 stopped, 1 normal, 2 suspended; `ServiceType` distinguishes YouBike 1.0, YouBike 2.0 and Moovo. Responses support `Last-Modified`/`If-Modified-Since`, a health query, and 304 when unchanged. [TDX Bike v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2cc9b888-a592-496f-99de-9ab35b7fb70d)

**Verified update declaration.** The current TDX availability catalog says every minute, free, API-key required, and not quality checked. [Government catalog: public-bike availability](https://data.gov.tw/dataset/177480)

**Verified timestamp semantics.** TDX support states `UpdateTime` is when TDX interfaces with YouBike, whereas `SrcUpdateTime` is when that station's data changed due to a rental/return. Therefore a large `UpdateTime - SrcUpdateTime` gap can mean an unchanged station, not a broken feed. [TDX administrator response on Bike timestamps](https://tdx.transportdata.tw/topic/detail/f5ab4264-abd2-4879-804f-3d7cc2bbd5dd)

**Verified direct-source fallback.** Taipei publishes a JSON YouBike 2.0 feed every minute with station number, names/address, service state, rental count, return count, coordinates, capacity and multiple timestamps. [Taipei YouBike 2.0 dataset](https://data.gov.tw/dataset/137993) New Taipei publishes equivalent official fields but declares a five-minute update frequency. [New Taipei YouBike 2.0 dataset](https://data.gov.tw/dataset/146969)

### Identifier and freshness contract

**Recommended contract.** Persist `StationUID` as identity and `(StationUID, observed_at)` as the observation key. Do not key on station name, address, or bare `StationID`. Treat station capacity and location as versioned static data because stations can be installed, moved, resized, suspended, or retired.

Use TDX `UpdateTime` (and HTTP response time) to judge feed freshness. Use `SrcUpdateTime` to understand station-level change, not as a sole stale-feed alarm. Persist both.

### Reliable Arrival use

**Recommended contract.** A YouBike Leg can support Reliable Arrival only if both the pickup and planned return stations are normal and fresh, with safety-stock thresholds applied to rental bikes and return slots. Counts are snapshots, not held inventory: even a fresh count of one is not reliable. Start with a configurable minimum of two bikes and two return slots, then calibrate by station/time-of-day using historical observations. If e-bike is required, use the detailed e-bike count rather than the aggregate.

No authoritative source promises that a displayed bike or slot will remain available until the rider arrives. The product must describe this as current feasibility with a buffer, never a guarantee.

## Contract 3: Metro

### Static model

**Verified.** TDX Rail v2 supplies lines, stations, station order, transfers, exits/facilities, fares, travel times, first/last trains, frequency, and station timetable. Relevant operator codes in the schema are `TRTC` (Taipei Metro), `NTMC` (New Taipei Metro), `NTDLRT` (Danhai LRT), and `NTALRT` (Ankeng LRT). Availability varies by endpoint, so code must validate the endpoint enum instead of assuming every operator supports every resource. [TDX Rail v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/268fc230-2e04-471b-a728-a726167c1cfc)

The station schema includes `StationUID`, `StationID`, names, WGS84 position, address, jurisdiction codes, `SrcUpdateTime`, TDX `UpdateTime`, and `VersionID`. Station timetable includes `LineID`, `StationID`, direction, destination, a timetable array, service days/special days, source/update timestamps and version. TDX explicitly excludes Taipei Metro's Wenhu Line from station timetables and directs clients to Frequency. Official Taipei Metro independently states Wenhu has no fixed timetable and may add trains dynamically. [TDX Rail v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/268fc230-2e04-471b-a728-a726167c1cfc), [Taipei Metro timetable page](https://web.metro.taipei/pages/tw/timetables/013)

**Verified completeness risk.** An official TDX support thread records a case where a newly effective Taipei Metro timetable had not yet appeared in TDX. This is evidence of possible source-to-TDX lag, not a quantified latency guarantee. [TDX support: Taipei Metro timetable update](https://tdx.transportdata.tw/topic/detail/7599fd69-8c6c-4fc2-b127-ac859ccddde9)

**Recommended identifier contract.** Use `StationUID` where present. For resources such as LiveBoard that carry only local codes, namespace keys explicitly:

```text
metro_station_key = (RailSystem, StationID)
metro_line_key = (RailSystem, LineID)
metro_leg_key = (RailSystem, LineID, direction, origin_StationID, destination_StationID)
```

Do not merge stations across operators by ID or name. Maintain an explicit, reviewed interchange mapping for Walking/Transfer connectors.

### Live Status and disruption limits

**Verified.** LiveBoard's formal schema includes station, line, destination, service status, `EstimateTime` in minutes, `SrcUpdateTime`, and `UpdateTime`, but its operator enum is only TRTC/KRTC/TYMC/KLRT. The TDX administrator confirms the critical TRTC exception: Taipei Metro provides records only when the train has arrived, with `EstimateTime=0`. Thus schema capability does not equal TRTC data capability. [TDX Rail v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/268fc230-2e04-471b-a728-a726167c1cfc), [TDX administrator response on Taipei Metro LiveBoard](https://tdx.transportdata.tw/topic/detail/d2f5e1d0-ef2f-4a6d-a4ab-f6293c959138)

**Verified.** The Alert endpoint includes TRTC but not NTMC/NTDLRT/NTALRT. An official TDX response confirmed that a brief Taipei Metro incident was represented by the API during the affected minutes, showing the feed can capture short events; it does not prove an SLA or completeness. [TDX support: TRTC Alert incident](https://tdx.transportdata.tw/topic/detail/d616188e-387b-4d60-af50-f4530c07cc53)

**Recommended contract.** For TRTC, calculate Metro time from official timetable/frequency plus published station-to-station and Transfer times, then invalidate or widen the result when Alert is abnormal/stale. Do not label the train arrival component as Live Status. For NTMC/NTDLRT/NTALRT, do not qualify a Metro-critical Journey Option as Reliable Arrival until an authoritative, freshness-bearing operational-status source is added and tested. Static Metro Legs remain valid Journey Options with an explicit schedule-based confidence state.

## Freshness, latency, polling, and availability

### Verified upstream facts

- TDX declares one-minute publication for the cataloged Bus N1 and Bike availability APIs. This is an update frequency, not an end-to-end latency bound. [Bus N1 catalog](https://data.gov.tw/dataset/161159), [Bike availability catalog](https://data.gov.tw/dataset/177480)
- TDX resources expose source and platform timestamps; OpenAPI also documents `Last-Modified`/`If-Modified-Since`, 304 responses, and a `health=true` query. [TDX Bus v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2998e851-81d0-40f5-b26d-77e2f5ac4118), [TDX Bike v2 OpenAPI](https://tdx.transportdata.tw/api-service/swagger/basic/2cc9b888-a592-496f-99de-9ab35b7fb70d)
- Unauthenticated visitor use is browser-only/basic and capped at 20 requests per source IP per day. TDX currently describes a basic member allowance of three points per month (approximately 3,000 calls); basic APIs account at 1,500 calls or 150 MB per point, while historical APIs account at 10 calls or 20 MB per point. Exact per-key request-rate limits depend on subscription. Token issuance is capped at 20 calls per IP per minute, so tokens must be cached. [TDX API guide](https://tdx.transportdata.tw/api-service/swagger), [TDX official homepage allowance notice](https://tdx.transportdata.tw/), [official TDX sample-code authentication notes](https://github.com/tdxmotc/SampleCode), [TDX Bus historical OpenAPI](https://tdx.transportdata.tw/api-service/swagger/historical/39472fde-f790-42b9-9c8b-88282118e6b1)
- TDX allows up to three API keys per member. [TDX service-use guide](https://tdx.transportdata.tw/about/service)
- TDX publishes planned-maintenance notices warning that service may be unstable, and has announced temporary unavailability for specific service modules. This is availability history, not a measured uptime series. [TDX maintenance example](https://tdx.transportdata.tw/news/detail/a0154845-98b6-48c-8844-72f690226484), [TDX MaaS outage notice](https://tdx.transportdata.tw/news/detail/72112abb-8765-4a77-a17d-f6cb59cb9d53)

### Unknowns

- No official numeric uptime SLA, response-latency SLO, source-to-TDX latency bound, retention guarantee, or completeness guarantee was found.
- No official promise was found that every scheduled update changes every record.
- Subscription rate limits and prices are plan-dependent and can change; they must be read from the account/pricing page at deployment time, not hard-coded from this report.
- Exact continuity and sampling cadence of historical Taipei/New Taipei N1, A1/A2, Bike availability, Metro Alert and LiveBoard observations remain unverified without authenticated extraction.

### Recommended operational contract

The following values are Via policy, not upstream guarantees:

1. Poll each city-level Bus N1 and Bike availability endpoint no faster than once per 60 seconds; use `$select`/`$filter`, compression, `If-Modified-Since`, and shared server-side caching. Never make one upstream request per rider or per stop.
2. Record `fetched_at`, HTTP status, response duration, `Last-Modified`, payload hash/count, `SrcUpdateTime`, `UpdateTime`, and max/median observed age. Cache access tokens and refresh on expiry, not per request.
3. Initial freshness policy for TDX one-minute Live Status: fresh at age `<=120s`, degraded at `120–180s`, unavailable after `180s`. Calibrate from observed distributions. Direct New Taipei fallbacks must use their slower declared cadence (for example, Bike may require a roughly ten-minute hard cutoff rather than three minutes).
4. Empty payload, missing selected identifier, repeated unchanged `UpdateTime`, health failure, 401/429/5xx, and schema drift are distinct states. Do not translate all of them into zero vehicles/bikes or normal operation.
5. Preserve last-known observations for diagnostics, but never silently use an expired observation to assert Reliable Arrival.
6. Add circuit breakers and retain the direct city adapters for incident diagnosis. Switching to them should be deliberate because identifiers, fields and cadences differ.

## Licensing and product obligations

**Verified.** The relevant TDX and city catalog entries use Taiwan's Open Government Data License 1.0. It grants royalty-free, non-exclusive, irrevocable use without purpose/time/territory restriction, allows modification and sublicensing, and is compatible with CC BY 4.0. It requires explicit attribution to the providing organization; failure to attribute means the licence was never obtained. It excludes patent and trademark rights. Providers may cease all or part of a dataset for stated reasons without compensation, and disclaim warranties and liability. [Open Government Data License 1.0](https://data.gov.tw/license)

**Recommended contract.** Ship a data-attribution surface naming MOTC/TDX and the underlying city/operator dataset providers, link the licence, preserve dataset/version metadata internally, and do not use agency/YouBike marks as though Via were endorsed. Legal review should confirm the final attribution wording and whether the UI, app-store listing, or an in-app data-sources page is the appropriate visible location.

## Historical testing plan for Reliable Arrival

**Verified availability.** TDX says members can request historical files or use historical APIs. Its historical catalog contains 616 public-transport services, including city Bus, Metro and public Bike domains; listed data types include real-time arrivals and real-time remaining Bike capacity. Bus historical OpenAPI says data is available after May 2021, excluding the current day. Historical access costs materially more points per call than basic access. [TDX historical-service catalog](https://tdx.transportdata.tw/data-service/historical), [TDX Bus historical OpenAPI](https://tdx.transportdata.tw/api-service/swagger/historical/39472fde-f790-42b9-9c8b-88282118e6b1)

**Inference.** There is enough official historical surface to bootstrap Bus and YouBike calibration, but not enough public metadata to claim a complete, gap-free replay dataset for the exact Taipei/New Taipei endpoints. Metro history cannot repair the missing forward TRTC LiveBoard signal.

**Recommended verification spike before model claims:**

1. With a member key, enumerate exact historical services and supply windows for Taipei/New Taipei N1, A1/A2, Bike availability, TRTC Alert and any Metro arrival product.
2. Download several normal weekdays, weekends, rain days, holidays and officially announced disruptions. Measure timestamp cadence, missing intervals, identifier churn, duplicates, null/sentinel distribution, and cross-feed join success.
3. Reconstruct prediction-time snapshots only; never leak later observations into the decision point.
4. Define outcome labels: Bus actually reaches the boarding stop before buffer expiry; Bike pickup and return remain feasible at the rider's predicted arrival; Metro schedule remains valid under alerts. TDX N1 estimates alone are predictions, not ground-truth arrivals, so use A2/stop events where available to derive Bus outcomes.
5. Backtest by Journey Option and critical Transfer, reporting coverage separately from correctness. A policy that declines to assert Reliable Arrival on stale/missing data should not be scored as a wrong prediction.
6. Start continuous first-party observation capture immediately. Store raw licensed payloads plus normalized rows and schema/version hashes; this becomes the only test corpus whose sampling behavior Via controls.

## Acceptance criteria for implementation

- Every normalized record retains source, endpoint version, operator/city namespace, upstream identifiers, source/platform timestamps, fetch timestamp and raw status semantics.
- Journey Option construction never joins on display names.
- Bus N1 sentinels/nulls and Bike service states are modeled explicitly, not coerced to numeric zero.
- Metro capability is operator-specific: TRTC LiveBoard is not treated as predictive; New Taipei Metro is not treated as having TDX Alert/LiveBoard.
- Reliable Arrival has a machine-readable reason when unavailable or degraded (`stale`, `missing`, `schedule_only`, `operator_not_covered`, `disrupted`, `insufficient_bike_buffer`, `insufficient_return_buffer`, `rate_limited`, `source_error`).
- Upstream update frequency is never described as an SLA, and Reliable Arrival is never presented as a guarantee.
- Attribution and licence link are present before public release.

## Final fact/inference/unknown split

**Verified:** TDX v2 covers both cities for Bus and Bike with one-minute catalog update frequency; fields and UID rules are machine documented; direct city feeds exist under the same open-data licence; TRTC LiveBoard lacks forward estimates; New Taipei Metro is absent from TDX v2 LiveBoard/Alert enums; official historical service families exist; TDX does not quality-check the two core catalog datasets.

**Inference:** TDX is the lowest-risk canonical normalization layer, but a Via-owned observation/freshness/reliability layer is required. Static Metro plus disruptions can support cautious schedule-based feasibility, but not live train prediction. Bus and Bike historical data are likely sufficient to begin calibration after continuity checks.

**Unknown:** numeric SLA/latency, per-record completeness, exact historical continuity, New Taipei Metro live operational coverage outside the reviewed TDX contract, and identifier stability across all upstream migrations.

**Recommended contract:** adopt TDX UIDs and operator namespaces; poll/capture centrally; gate Reliable Arrival on fresh usable Live Status plus buffers; degrade visibly; keep direct city adapters for diagnosis/fallback; validate historical supply before making accuracy claims.
