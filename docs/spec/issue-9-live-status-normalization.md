# Issue #9 — Live Status normalization model

Decision date: 2026-08-29  
Status: Superseded
Superseded by: ADR 0002, ADR 0006, and the current provider interfaces under `server/src/providers/`.

## Decision

Via owns a provider-neutral, typed network model. Every durable entity receives a stable opaque Via ID; TDX and city-feed identifiers are versioned aliases kept behind the ingestion boundary. Routing uses Via IDs only, and clients receive only rider-relevant normalized facts and public attribution.

Static network revisions and request-local Live Status evidence are separate. Each Planning Request reads one immutable `TransitNetwork` revision and freezes the normalized realtime values collected for its scheduled candidates. All Journey Options calculated for the request use that same evidence context.

The normalized Live Status families are:

1. Arrival Estimate for a Bus at a Boarding Point;
2. Service Condition for Bus or Metro disruption state;
3. Bike Availability for rental and return feasibility at a YouBike Station; and
4. feed health, which describes whether a source can currently support fresh observations.

Metro timetable and frequency data remain scheduled network data. They do not become predictive Live Status. In particular, TRTC LiveBoard records with `EstimateTime = 0` do not support a future Arrival Estimate, and the reviewed TDX contract does not imply Live Status coverage for New Taipei Metro.

This model implements the source findings in [issue #4](../research/issue-4-transit-data-contracts.md) and supplies the pinned snapshot required by the routing decision in [issue #8](../research/issue-8-routing-algorithms.md).

## Hard boundaries

- Provider payloads enter only through adapters.
- Provider IDs, field names, status codes, direction codes, sentinel values, and schemas never appear in routing or client contracts.
- Display names, coordinates, and proximity never establish identity.
- Missing or unknown values never become zero, normal, or on time.
- A source's advertised update frequency is not a freshness guarantee or service-level agreement.
- Live Status is evidence about the observed moment. It is not a reservation, forecast of YouBike stock, or guarantee of Reliable Arrival.
- No normalized transport observation is associated with a Planning Request or rider.

## Identity model

All IDs are typed and opaque. An ID from one entity type cannot be used as another type, even if their serialized values happen to match.

| Entity | Identity and role |
| --- | --- |
| Operator | The organization operating a Bus, Metro, or YouBike service. It is distinct from the data provider. |
| Stop Place | A named Bus or Metro area that groups related Boarding Points. |
| Boarding Point | The exact or best-known place where boarding or leaving occurs. It records whether its known location is a stop pole, platform, line-and-direction area, or station area. |
| YouBike Station | A rental location with versioned position, capacity, and supported bike classes. It is not a Boarding Point. |
| Transit Service | A rider-recognizable Bus or Metro service. |
| Service Pattern | One ordered sequence of Boarding Points for a Transit Service, direction, and variant. Provider direction values are adapter inputs, not canonical direction values. |
| Scheduled Trip | One planned traversal of a Service Pattern on a Service Day. It is optional for feeds that cannot identify a trip reliably. |
| Vehicle | A physical Bus or train when stable identity is available. Live Status never requires it. |
| Service Calendar | The recurring active days and explicit added or removed Service Days for scheduled service. |
| Observation | One immutable normalized statement that Via received from a source. |
| Network Snapshot | One immutable selection of network, schedule, Live Status, and feed-health versions. |

### Provider aliases

The ingestion registry maps this internal shape:

```text
(provider, entity kind, authority namespace, upstream key, valid interval)
    -> Via entity ID
```

One Via entity may have several aliases across providers or source versions. Alias changes do not change the Via ID when the real entity remains the same. A real split or merge creates new Via IDs and retires the old IDs; retired IDs are never reused. Splits, merges, and uncertain matches require a reviewed crosswalk rather than automatic name or distance matching.

The canonical IDs are intentionally not derived from TDX UIDs. TDX remains the canonical Taipei–New Taipei ingestion boundary, while Via identity remains stable if that boundary or an upstream authority changes.

## Stop, station, and Transfer relationships

```text
Stop Place
    contains 1..n Boarding Points

Boarding Point
    -- directed Transfer --> Boarding Point
    -- Walking connector --> YouBike Station

YouBike Station
    -- bicycle movement --> YouBike Station
```

- A Bus pole is normally one Boarding Point. Opposite directions remain separate even when names and coordinates are similar.
- A Metro station is a Stop Place. Known platforms are Boarding Points. When the source is less precise, Via may use a line-and-direction or station-area Boarding Point, but it must record that lower location precision instead of claiming an exact platform.
- A confirmed Bus station group may become a Stop Place containing its Bus Boarding Points. The upstream group is evidence, not automatic authority to merge.
- Interchanges between operators remain separate Stop Places unless a reviewed mapping says they are one rider-facing place.
- Every Transfer is explicit and directed. It may carry minimum duration, accessibility facts when known, validity dates, and provenance. Name or coordinate equality never creates a Transfer.
- A YouBike Station remains a separate entity connected through Walking. It is never folded into a Bus or Metro Stop Place.

## Time and freshness

Every Live Status observation keeps separate time meanings:

| Time | Meaning |
| --- | --- |
| `measured_at` | When the operator or source measured the underlying state, when supplied. |
| `published_at` | When the upstream provider published or refreshed the record, when supplied. |
| `received_at` | When Via successfully received the payload. Always present. |
| `normalized_at` | When Via produced the normalized observation. Audit metadata only; it cannot prove freshness. |

The adapter documents the exact meaning and confidence of each upstream timestamp. It never copies an ambiguous upstream field into a generic `updated_at` property.

Freshness is calculated from a versioned policy by observation family, source capability, and use. The result is `fresh`, `stale`, or `unknown`, with the evaluated time, policy version, and reason. Numeric limits belong to the server freshness policy decided in issue #12, not to entity identity.

Important rules:

- A Bus countdown is anchored to the source measurement or transmission time and normalized to an absolute predicted instant. Via may decrement it from that anchor; it does not treat the received number as timeless.
- YouBike `SrcUpdateTime` can mean the last rental or return changed the station record. An unchanged station can therefore have an old source-change time while the feed remains healthy. Bike feed freshness uses provider publication, successful receipt, and feed health; the source-change time remains useful evidence but is not the sole stale-data clock.
- Feed health records the last successful receipt, failure or rate-limit state, and the provider's declared cadence. Declared cadence informs policy but is not an SLA.
- A stale observation can remain stored for diagnosis and replay, but it cannot silently count as fresh evidence.

## Network Snapshot

A Network Snapshot contains:

```text
snapshot_id
created_at
static_network_revision
schedule_revision
live_cutoff
selected_observation_ids
feed_health_ids
freshness_policy_version
```

Snapshot construction selects only observations received at or before `live_cutoff`. Different sources may have different measurement times; the snapshot preserves those times and their individual freshness results. “One snapshot” means one consistent selection boundary, not a false claim that all providers measured the city simultaneously.

A Planning Request pins one `snapshot_id` for its calculation. Refreshing results creates a new Planning Request calculation against a newer snapshot. Existing Journey Options are not mutated in place. Replaying the same request inputs, schedule revision, policy version, and snapshot must produce the same normalized routing inputs.

## Arrival Estimate

An Arrival Estimate contains:

```text
observation_id
boarding_point_id
transit_service_id
service_pattern_id
service_day
scheduled_trip_id?       // absent when the source cannot identify it safely
vehicle_id?             // absent when identity is missing or unstable
scheduled_arrival_at?
predicted_arrival_at?
state
times
freshness
provenance_ref
```

Canonical states are:

- `predicted`;
- `at_boarding_point`;
- `not_departed`;
- `skipped`;
- `last_service_passed`;
- `not_operating`;
- `unknown`.

The adapter preserves the raw status in provenance and performs an explicit mapping. TDX N1 nulls and sentinel meanings remain distinct; none is coerced to a zero-second arrival. A prediction is matched by Service Pattern and Boarding Point. Scheduled Trip and Vehicle are refinements, not required join keys.

Schedule-only time is not an Arrival Estimate. It is scheduled network data with `schedule_only` evidence. A2 or vehicle-position observations may corroborate an Arrival Estimate, but they do not become Via predictions unless Via later owns and validates a separate prediction model.

## Service Calendar and Metro

Each Service Calendar has an operator time zone, recurring active days, an effective interval, and explicit added or removed Service Days. Scheduled times are offsets from the start of the Service Day and may exceed 24 hours, so service after midnight can still belong to the preceding Service Day.

Schedule revisions are immutable and effective-dated. A live `not_operating` state or Service Condition does not rewrite the Service Calendar; it overlays the pinned schedule inside the Network Snapshot.

Metro handling is capability-specific:

- TRTC timetable, frequency, travel, and Transfer data are scheduled inputs.
- A TRTC LiveBoard record that only reports a train at the station may be retained as a present-state observation, but it cannot create a future predictive Arrival Estimate.
- New Taipei Metro remains `operator_not_covered` for TDX predictive arrivals and alerts until a tested authoritative adapter establishes otherwise.
- The absence of a Service Condition never means normal service when the relevant alert feed is unsupported, stale, or unavailable.

## Bike Availability

Bike Availability contains:

```text
observation_id
youbike_station_id
rental_state            // enabled | disabled | unknown
return_state            // enabled | disabled | unknown
available_bikes?        // aggregate count when known
available_return_slots? // count when known
available_by_class      // general, electric, or future classes when known
times
freshness
provenance_ref
```

Counts are non-negative integers or unknown. Zero is a valid observed value and is different from unknown. When aggregate and bike-class counts are both present, Via retains both and validates obvious contradictions without inventing replacement values.

Static capacity belongs to the versioned YouBike Station. Current bike and return-slot counts belong to Bike Availability. Rental and return permission gate the corresponding action even when a positive count is present.

Bike Availability is a query-time snapshot, not reserved inventory or a prediction at the rider's future arrival. The Reliable Arrival layer applies configurable safety thresholds to the actual counts. The MVP's initial three-bike and three-return-slot thresholds remain policy outside this normalization model.

## Service Condition

A Service Condition has an effective interval, normalized severity and state, rider-facing localized text when licensed, and one or more explicit scopes:

- operator;
- Transit Service;
- Service Pattern;
- Stop Place; or
- Boarding Point.

Canonical states include `disrupted`, `suspended`, and `closed`. Normal service is inferred only when the adapter contract supports that conclusion and the source is fresh. Raw alert categories and operator codes remain in provenance.

## Evidence and degradation

Routing receives a normalized evidence classification for each critical Leg or action:

- `live`: fresh, semantically usable Live Status supports it;
- `schedule_only`: a usable schedule exists but predictive Live Status does not; or
- `unavailable`: neither live nor permitted scheduled evidence supports it.

Machine-readable reasons include:

```text
stale
missing
schedule_only
operator_not_covered
disrupted
insufficient_bike_buffer
insufficient_return_buffer
rate_limited
source_error
```

The Reliable Arrival and ranking layers decide how those classifications affect Journey Option feasibility and order. The normalization layer supplies facts and reasons; it does not hide degraded evidence or make a guarantee.

## Provenance

Each observation has an internal provenance reference to:

- provider and dataset;
- endpoint/schema version and adapter version;
- operator or city authority namespace;
- upstream entity identifiers;
- raw status and timestamp fields;
- receipt metadata and payload hash;
- raw licensed payload location or capture reference;
- licence and required attribution; and
- normalization warnings or validation failures.

Raw payloads and provenance are operational records, never Product Events. They must not contain a Planning Request, Journey, rider location, or stable rider identifier.

Routing may retain the opaque provenance reference for explanation and diagnosis, but it cannot branch on a provider field. Client contracts expose only observation time, normalized freshness/warnings, and public attribution required for the Journey Option. They never expose provider entity IDs, raw statuses, endpoint names, or payload shapes.

## Adapter mappings for the current sources

| Upstream concept | Via mapping |
| --- | --- |
| TDX Bus `StopUID` | Provider alias for a Boarding Point. |
| TDX Bus station/group IDs | Evidence for a Stop Place relationship; never an automatic merge. |
| TDX Bus `SubRouteUID` plus direction and ordered stops | Provider aliases and inputs for a Service Pattern. |
| TDX Bus N1 | Arrival Estimate after explicit null, sentinel, time-anchor, and status mapping. |
| TDX Bus plate number | Optional Vehicle alias; never required identity or a client field. |
| TDX Metro station and line IDs | Provider aliases for Stop Places, Boarding Points, Transit Services, and Service Patterns according to source precision. |
| TDX Metro timetable/frequency | Scheduled trips or frequency windows attached to a Service Calendar. |
| TDX Metro Alert | Service Condition only for operators covered by that endpoint. |
| TDX YouBike `StationUID` | Provider alias for a YouBike Station. |
| TDX YouBike availability | Bike Availability with separate source-change, publication, and receipt times. |

Direct Taipei and New Taipei feeds use their own adapters and aliases. They may corroborate or replace a failing canonical ingestion source, but their identifiers never replace Via IDs.

## Validation contract

The implementation is conformant only when automated fixtures prove that:

- changing an upstream ID for the same real entity preserves the Via ID through a reviewed alias update;
- opposite-direction Bus poles with the same name remain distinct Boarding Points;
- same-named Metro stations across operators do not merge without an explicit mapping;
- provider direction values cannot leak into routing as unexplained numeric codes;
- Bus nulls and every N1 sentinel map to distinct canonical states;
- an optional Vehicle or Scheduled Trip never prevents a valid Bus Arrival Estimate join;
- service after midnight resolves against the correct Service Day;
- an old YouBike source-change time does not by itself mark a successfully refreshed feed stale;
- disabled rental or return state overrides a positive count, while unknown remains different from zero;
- TRTC present-only LiveBoard data cannot become a predictive Arrival Estimate;
- unsupported New Taipei Metro live coverage produces `operator_not_covered`, not normal service;
- stale, missing, rate-limited, and source-error fixtures produce explicit degradation reasons;
- the same pinned Network Snapshot yields the same normalized routing inputs; and
- serialized routing and client fixtures contain no upstream IDs, raw status values, or provider schema fields.

## Consequences for other Wayfinder decisions

- Issue #3 can define a small shared Journey Planning API from normalized Journey Options without importing TDX types. It should expose a snapshot/version token, observation times, actual YouBike counts, warnings, and attribution.
- Issue #6 can assign refresh display behavior to clients without giving them provider adapters or provider identifiers.
- Issue #11 can define Reliable Arrival and ranking thresholds over evidence classifications, freshness, Transfer margins, and Bike Availability.
- Issue #12 must choose refresh ownership, numeric freshness budgets, snapshot storage, observation retention, and safe behavior when only part of a snapshot can refresh.
- Taiwan-wide expansion adds adapters and alias mappings. It does not change routing identity or client contracts.
