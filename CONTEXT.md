# Journey Planning

The language used to describe real-time multimodal journey planning for riders using public transportation and YouBike.

## Language

**Journey**:
The rider's complete movement from an origin to a destination.
_Avoid_: Route, trip

**Journey Option**:
One feasible multimodal alternative offered for a Journey.
_Avoid_: Way, route option

**Leg**:
One continuous segment of a Journey using a single mode of travel.
_Avoid_: Step, segment

**Travel Mode**:
The means used for a Leg: Walking, Bus, Metro, or YouBike. Bus, Metro, and YouBike are rider-selectable filters; Walking is always available as a connector and is not shown as a filter.
_Avoid_: Tool, transportation type

**Transfer**:
The static public-transit interchange between two Boarding Points, including the
time and failure risk involved. A YouBike path is an endpoint connector, not a
Transfer.
_Avoid_: Change

**Guidance**:
Static, human-readable instructions for a selected Journey Option. Guidance does not track or advance through Legs.
_Avoid_: Navigation, live guidance

**Live Status**:
Time-stamped operational information such as a bus arrival estimate, available-bike count, or return-slot count.
_Avoid_: Live Observation, real-time data

**Planning Inputs**:
The immutable static `TransitNetwork` revision plus the request-local realtime facts collected for one Planning Request. Every Journey Option from the request uses the same static revision and frozen evidence context.
_Avoid_: Current data, latest data

**Stop Place**:
A named Bus or Metro area that groups one or more Boarding Points without hiding where boarding or leaving actually occurs.
_Avoid_: Station group, stop group

**Boarding Point**:
The exact or best-known place where a rider boards or leaves a Bus or Metro service.
_Avoid_: Platform node, stop node

**Service Day**:
The local operating date to which scheduled Bus or Metro service belongs, including service that continues after midnight.
_Avoid_: Calendar day, operating day

**Arrival Estimate**:
Live Status that describes whether and when a Bus is expected at one Boarding Point. It may be unavailable or have a non-time state such as not departed, skipped, or no longer operating.
_Avoid_: Countdown, ETA record

**Service Condition**:
Live Status that describes a disruption, suspension, closure, or other operational change affecting Bus or Metro service.
_Avoid_: Alert record, incident feed

**Bike Availability**:
Live Status that separately reports whether a YouBike Station can rent and accept bikes, together with the known bike and return-slot counts.
_Avoid_: Inventory, bike stock

**Freshness**:
Whether Live Status is recent enough for its configured use. Freshness is separate from whether the reported value last changed.
_Avoid_: Recency, updated state

**Reliable Arrival**:
An estimated arrival whose critical Legs and Transfers remain feasible under current operational data and safety buffers.
_Avoid_: Fastest route, guaranteed arrival

## Transfer architecture

Static public-transit transfers are normalized into `TransitNetwork.transfer_edges`,
which is the only persistent source of truth. `TransferShortcutIndex` is an
in-memory index derived from the validated network at runtime. There is no
separate transfer preprocessing phase or `transfer-shortcuts.json` artifact.
Public-transit transfer reconstruction uses the static duration and synthetic
walking geometry; it never calls Valhalla or a street router. YouBike remains
only query-time origin/destination connectivity and never appears in static
transit transfer edges.

## Timing and reliability

Every transit mode is resolved realtime-first with static fallback. Realtime
departures are used only when fresh and usable; empty, stale, missing, future,
or failed realtime resolution falls back to static timing without removing an
otherwise valid journey.

`EffectiveTrip.timing_basis` is the source of truth for journey reliability.
`RealtimePrediction` and `DelayAdjustedSchedule` are realtime-backed;
`Schedule` and `FrequencyEstimate` are static-backed. A journey is
`Reliable` only when every transit timing fact and required YouBike
availability fact is fresh realtime. Otherwise, a valid journey is
`Feasible`.

## Transit identity decision

Via currently uses TDX as the authoritative source for Taiwan transit static
and realtime data, including expected nationwide expansion. The normalized
`TransitNetwork` and realtime adapter construct the same strongly typed domain
IDs directly from TDX identifiers. Via does not maintain a provider-neutral
canonical identity registry or persist aliases while TDX is the sole current
transit provider. Revisit reconciliation only if Via combines multiple
providers that describe the same logical entities.

## Privacy and Telemetry

**Planning Request**:
A one-time request that contains only the selected origin, destination, Travel Mode filters, locale, and departure setting needed to calculate Journey Options.
_Avoid_: Search record, trip record

**Operational Event**:
An anonymous measurement of service behavior, such as latency, an error, stale Live Status, or the number of returned Journey Options. It contains no location, Journey details, or stable rider identifier.
_Avoid_: Usage event, analytics event

**Product Event**:
A consented measurement of how a rider uses Journey Planning, such as selecting a Journey Option or expanding Guidance. It uses coarse categories and does not identify the rider across app sessions.
_Avoid_: Tracking event, behavioral profile

**Feedback Report**:
An explicit report that a rider sends about a Journey Planning problem. It excludes the origin, destination, and Journey details unless the rider separately chooses to include them after a preview.
_Avoid_: Support ticket, automatic report

## Infrastructure decisions

TDX static snapshot fetching keeps its small internal timing policy. The
request limiter, deterministic retry schedule, `Retry-After` handling, progress
events, and snapshot reuse/resume are one coupled workflow; introducing a
generic retry middleware would add adapters without removing that behavior.

`TdxCredentials` remains a redacted internal `String` container. Via has no
constant-time secret comparison, and a `SecretString` wrapper would add access
noise without changing the existing formatting boundary. The single Axum
request-timeout middleware also remains internal because it returns Via's
structured `request_timeout` response directly.

Geographic calculations use the shared `RequestCoordinate` helpers. Spatial
candidate selection and routing thresholds intentionally use the existing
rounded local planar approximation; offline straight-line estimates and CLI
distance display intentionally use Haversine distance. These named semantics
avoid a new geographic dependency while preserving the different accuracy
requirements.
