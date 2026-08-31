# Issue #3 — Shared Journey Planning API contract

Decision date: 2026-08-29
Status: Accepted

## Decision

Via exposes one provider-neutral, server-authoritative JSON contract for iOS and Android:

```http
POST /v1/journey-plans
Content-Type: application/json
Accept: application/json
```

The endpoint accepts one ephemeral Planning Request and returns one complete result calculated from one immutable Network Snapshot. The same request, snapshot, static and schedule revisions, and policy versions produces the same ordered Journey Options and semantic facts on both clients.

The contract is specified with OpenAPI and uses `snake_case` JSON names. No request field or response type contains an Apple, Google, TDX, city-feed, or other upstream identifier or native-framework object. MapKit and the Android map SDK remain client adapters.

Every response uses `Cache-Control: no-store`. Planning Requests, Journey Options, coordinates, display text, and Journey details must not enter CDN caches, application logs, traces, error reports, or retained server records.

## Planning Request

### Example

```json
{
  "origin": {
    "coordinate": {
      "latitude": 25.04776,
      "longitude": 121.51706
    },
    "display_text": "Taipei Main Station"
  },
  "destination": {
    "coordinate": {
      "latitude": 25.03396,
      "longitude": 121.56447
    },
    "display_text": "Taipei 101"
  },
  "selected_modes": ["bus", "metro", "youbike"],
  "locale": "zh-Hant-TW",
  "departure_setting": {
    "type": "depart_now"
  }
}
```

### Selected place

`origin` and `destination` each contain only:

| Field | Type | Contract |
| --- | --- | --- |
| `coordinate.latitude` | JSON number | Finite WGS84 latitude in `[-90, 90]`. |
| `coordinate.longitude` | JSON number | Finite WGS84 longitude in `[-180, 180]`. |
| `display_text` | string | Client-normalized NFC text, trimmed, non-empty, at most 200 Unicode scalar values. It is display-only and never an identity key. |

The client creates this object only after the rider selects a place. It never sends raw search text, autocomplete history, map browsing, unused candidates, provider place identifiers, device identifiers, or a native `MKMapItem`/Android equivalent.

The server must not snap a coordinate and then silently replace `display_text`. Any server-derived Stop Place, Boarding Point, or YouBike Station appears separately in the result with a stable Via ID.

### Travel Mode filters

`selected_modes` is a non-empty set serialized as an array in canonical order:

1. `bus`
2. `metro`
3. `youbike`

Duplicates or unknown values make the request invalid. Walking is always available as a connector and is never accepted as a filter. The server treats the selected modes as the allowed set, not preferences: a Journey Option can use any selected subset, but it cannot use an unselected non-Walking mode.

### Departure setting

`departure_setting` is a tagged union:

| `type` | Additional field | Meaning |
| --- | --- | --- |
| `depart_now` | none | Resolve “now” once on the server when the Planning Request begins. |
| `depart_at` | `time` | Depart no earlier than the supplied instant. |
| `arrive_by` | `time` | Arrive no later than the supplied instant. |

`time` is an RFC 3339 timestamp with an explicit UTC offset. The server returns the effective resolved search time and applicable IANA time-zone identifier in the result metadata. The server owns Service Day interpretation; clients never derive it from a calendar date alone.

### Locale

`locale` is a well-formed BCP 47 language tag. The initial supported Via resource locales are `zh-Hant-TW` and `en`.

Via-owned Guidance, warnings, empty reasons, labels, and errors are not localized by the server. The server returns semantic codes and typed parameters; each native client renders its own localized text and accessibility labels.

Operator-authored content is represented as locale-tagged source variants with attribution. The client selects a variant in this order:

1. exact requested locale;
2. matching language;
3. Traditional Chinese;
4. first available source variant.

The text is never a control-flow key. Absence of a source translation is not permission to invent or machine-translate operator text.

## Successful result envelope

HTTP `200` returns exactly one of `journey_options` or `empty`, with common metadata:

```text
{
  "api_version": "1.0",
  "semantic_catalog_version": "1.0",
  "request_id": "req_opaque",
  "generated_at": "2026-08-29T10:08:00+08:00",
  "effective_search_time": "2026-08-29T10:08:00+08:00",
  "time_zone": "Asia/Taipei",
  "network_snapshot": {
    "id": "snapshot_opaque",
    "created_at": "2026-08-29T10:07:45+08:00",
    "live_cutoff": "2026-08-29T10:07:40+08:00",
    "static_network_revision": "opaque_revision",
    "schedule_revision": "opaque_revision"
  },
  "policy_versions": {
    "freshness": "1",
    "ranking": "1",
    "transfer_safety": "1"
  },
  "warnings": [],
  "journey_options": [<JourneyOption>]
}
```

`request_id` supports anonymous operational correlation and contains no encoded request data. It is not a rider, device, session, place, or Journey identifier.

The snapshot and policy metadata makes a calculation replayable without exposing internal database or provider identifiers. All Journey Options in the response use the same metadata.

## Journey Option

The server returns one to three Journey Options in final authoritative order. When none survives, the response uses the structured `empty` outcome instead. Clients never rerank results.

Each Journey Option contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | opaque string | Unique only within this response. |
| `matching_key` | opaque string | A provider-neutral semantic key used only to preserve selection across a full refresh. It must not be persisted as history or decoded by a client. |
| `class` | `reliable \| feasible` | Server-owned ranking class. |
| `label` | nullable enum | `best_now`, `fewer_transfers`, or `less_walking`. |
| `ranking_reasons` | semantic code array | Ordered machine-readable reasons; no generalized cost. |
| `departure_time` | RFC 3339 string | Calculated departure instant. |
| `arrival_time` | RFC 3339 string | Calculated arrival instant. |
| `duration_seconds` | nonnegative integer | Complete Journey duration. |
| `walking_seconds` | nonnegative integer | Total Walking time. |
| `transfer_count` | nonnegative integer | Number of Transfers. |
| `travel_modes` | enum array | Modes actually used, in first-use order. |
| `live_status_summary` | object | Server-composed status indicator and conservative observation time for the compact Journey Option summary. |
| `overview_geometry` | nullable geometry | Complete display path, when supplied. |
| `legs` | non-empty Leg array | Ordered continuous Legs. |
| `transfers` | Transfer array | Explicit transitions between consecutive service Legs, including referenced connector Legs. |
| `warnings` | warning array | Scoped nonblocking facts about this Journey Option. |

`matching_key` is stable only when the server determines that two options across Network Snapshots represent the same semantic Journey. A client compares it for the currently open result only. If no refreshed option matches, the client selects the refreshed response's first-ranked Journey Option.

`live_status_summary` contains `state`, semantic reasons, and nullable `observed_at`. Its `observed_at` is the oldest observation time among the Journey Option's required live evidence, so the compact client summary does not overstate the age of mixed-source evidence. When the Journey Option has no applicable live observation, `observed_at` is `null` and `state` is `not_applicable`. Clients render this server-composed summary and do not derive a status indicator from individual Legs.

## Geometry

Every Leg carries geometry. A Journey Option can additionally carry an overview so clients do not concatenate or simplify Legs differently.

```json
{
  "encoding": "polyline6",
  "points": "encoded_points"
}
```

`polyline6` means the Google encoded-polyline algorithm with six decimal places. Any coordinate array elsewhere in the contract is `[longitude, latitude]`. Distances are integer metres and durations are integer seconds. A different geometry algorithm, precision, or coordinate order requires a new explicit encoding value with a compatible client path or a new API major version.

## Leg and Transfer structure

### Common Leg fields

Every Leg contains:

- response-local opaque `id`;
- `mode`: `walking`, `bus`, `metro`, or `youbike`;
- `from` and `to` rider-relevant places, each with a display name, coordinate, place kind, and optional stable opaque Via ID;
- calculated `departure_time` and `arrival_time`;
- nullable `scheduled_departure_time` and `scheduled_arrival_time` where a schedule exists;
- `duration_seconds` and `distance_metres`;
- `geometry`;
- ordered `guidance`;
- mode-specific `details` whose discriminator equals `mode`;
- scoped `live_status` evidence; and
- scoped `warnings` and public attribution.

Calculated, scheduled, and live-estimated values are distinct fields. A client never infers whether a time is live from the timestamp itself.

### Mode-specific details

| Mode | Required rider-relevant details |
| --- | --- |
| `walking` | Start/end entrance or side-of-street facts when known. No Live Status requirement. |
| `bus` | Stable Via service and Boarding Point IDs, public service label, headsign, boarding/alighting names and coordinates, stop count when known, and normalized Arrival Estimate/Service Condition evidence. |
| `metro` | Stable Via line and Boarding Point IDs, public line label, direction/headsign, boarding/alighting names and coordinates, and normalized Service Condition evidence. Schedule/frequency calculation is not mislabeled as a future Arrival Estimate. |
| `youbike` | Stable Via pickup and return station IDs, names and coordinates, pickup and return action states, actual available-bike and return-slot counts, and Bike Availability evidence. |

Stable Via IDs are typed and opaque. Provider aliases, raw TDX/city codes, raw provider statuses, endpoint names, and ingestion details never cross this boundary.

### Transfer

Each Transfer links the service Leg being left to the service Leg being joined with `from_leg_id` and `to_leg_id`. It can span one or more explicit Walking connector Legs, whose IDs appear in `connector_leg_ids`. A Transfer contains:

- `duration_seconds` for the physical Transfer;
- `available_seconds` under the calculation;
- server classification `reliable` or `feasible`;
- semantic reasons such as `tight_transfer_margin`;
- optional geometry when the Transfer is not already fully represented by a Walking Leg; and
- scoped Guidance and warnings.

An impossible Transfer suppresses its Journey Option. Clients do not recompute Transfer feasibility.

## Guidance

Guidance is an ordered array of static semantic instructions attached to a Leg or Transfer:

```json
{
  "code": "board_bus",
  "required": true,
  "parameters": {
    "service_label": "307",
    "boarding_point_name": "Taipei Main Station"
  }
}
```

Each code's allowed parameter names, types, units, optionality, and accessibility intent are part of the versioned semantic catalog. Parameters are typed JSON scalars or contract-defined objects, never printf-style positional arguments or preformatted Via sentences.

Guidance is static and human-readable after native translation. It does not track rider location, advance through Legs, or imply navigation. Operator-authored text, when needed, is a separate `source_text_variants` array containing `locale`, `text`, and attribution.

## Live Status and Freshness

Each Live Status evidence object contains:

| Field | Contract |
| --- | --- |
| `kind` | `arrival_estimate`, `service_condition`, or `bike_availability`. |
| `evidence_type` | `live` or `schedule_only`. |
| `observed_at` | Nullable RFC 3339 instant at which the source fact applied. |
| `published_at` | Nullable RFC 3339 source publication instant. |
| `received_at` | RFC 3339 instant Via received the source payload. |
| `evaluated_at` | RFC 3339 instant Via evaluated Freshness. |
| `freshness.state` | `fresh`, `degraded`, `unavailable`, or `not_applicable`. |
| `freshness.reason` | Stable semantic code or `null`. |
| `freshness.policy_version` | Version used for evaluation. |

The contract never exposes a generic ambiguous `updated_at`. `normalized_at` is an internal audit fact and is not evidence of Freshness.

Required Live Status that is degraded or unavailable suppresses the affected Journey Option. `degraded` and `unavailable` remain representable for noncritical warnings and for clients truthfully aging a retained result after refresh failure. `not_applicable` covers scheduled/calculated facts, such as Walking or an operator capability that does not provide a future Arrival Estimate.

### Actual YouBike availability

A YouBike Leg returns pickup and return evidence separately:

```json
{
  "pickup": {
    "action_state": "enabled",
    "available_bikes": 8
  },
  "return": {
    "action_state": "enabled",
    "available_return_slots": 12
  }
}
```

`action_state` is `enabled`, `disabled`, or `unknown`. Counts are nonnegative integers or `null` when unknown. Zero is an actual observed value and is never converted to `null`; `null` is never converted to zero. A count is present only when it is an actual normalized source observation. Via never generates, estimates, or substitutes a plausible count.

Availability is time-stamped evidence, not a reservation or forecast. Permissions gate feasibility even when a count is positive. The ranking and eligibility thresholds remain server policy and are not recalculated by clients.

## Warnings and operator text

A warning is structured as:

```json
{
  "code": "low_pickup_bikes",
  "required": true,
  "severity": "caution",
  "scope": {
    "type": "leg",
    "id": "leg_opaque"
  },
  "parameters": {
    "available_bikes": 1
  },
  "source_text_variants": []
}
```

`severity` is `information` or `caution`; a blocking condition removes the Journey Option or fails the complete request rather than masquerading as a warning. `scope.type` is `result`, `journey_option`, `leg`, or `transfer`, and its `id` is omitted only for result scope.

Clients branch only on `code`, `required`, and typed fields. They do not branch on source text. Public operator attribution accompanies the relevant normalized fact without leaking adapter identifiers.

## Empty and error outcomes

### Successful empty result

When calculation succeeds but no feasible Journey Option remains, HTTP `200` returns common metadata, warnings, and:

```json
{
  "empty": {
    "code": "no_feasible_journey_option",
    "required": true,
    "parameters": {
      "selected_modes": ["bus", "metro"]
    }
  }
}
```

An empty result is not an error. Its code distinguishes recovery actions without exposing discarded Journey details.

### Error envelope

HTTP `400`, `503`, and `500` use the same version headers and this body shape:

```json
{
  "api_version": "1.0",
  "semantic_catalog_version": "1.0",
  "request_id": "req_opaque",
  "error": {
    "code": "invalid_coordinate",
    "required": true,
    "field": "/origin/coordinate/latitude",
    "parameters": {}
  }
}
```

The error contract is:

| HTTP status | Meaning |
| --- | --- |
| `400` | Invalid Planning Request. Identify the field with an RFC 6901 JSON Pointer, but never echo its sensitive value. |
| `503` | No coherent Network Snapshot can be pinned, or all permitted transport evidence is unavailable. Retry is appropriate. |
| `500` | Global planner failure or inconsistent shared internal data. Return no partial Journey Options. |

One invalid candidate Journey Option is discarded while valid Journey Options survive. Only a calculation-wide trust failure fails the complete request.

## Semantic codes and compatibility

`api_version` versions the JSON schema. `semantic_catalog_version` independently versions semantic code meanings and parameter contracts. Policy versions identify calculation rules and do not grant a client permission to reinterpret results.

Within v1:

- new optional fields can be added and must be ignored by older clients;
- existing field meaning, type, units, requiredness, coordinate order, or enum meaning cannot change;
- new enum values require an already-defined safe client fallback, otherwise they are breaking;
- adding a translation does not change the API or semantic catalog;
- changing a semantic code's meaning or parameter contract changes the catalog version;
- removing or renaming a field or supported code requires a new API major version; and
- deprecations must remain supported until every released client has migrated through an agreed release window.

Every semantic object declares `required`. An unknown object with `required: false` is ignored without changing known semantics. An unknown object with `required: true` makes the response unsupported: the client must not guess, silently omit the meaning, or present the result as fully understood. Errors and Guidance are required by default; informational warnings can be optional only when omission preserves all rider decisions and safety meaning.

A server contract change cannot ship until both released clients have a compatible handling path or the version transition safely routes them to a compatible major version.

## Refresh behavior

Refresh repeats the complete original Planning Request. For `depart_now`, the server resolves a new effective time. Every refresh pins a new immutable Network Snapshot and returns a complete, internally consistent replacement.

The client sequence is:

1. keep the previous complete result visible and enter `refreshing`;
2. repeat the Planning Request;
3. on success, atomically replace all metadata and Journey Options;
4. preserve selection only when a new Journey Option has the same `matching_key`; otherwise select the first-ranked option; and
5. on failure, retain the previous result, leave `refreshing`, and show its truthful age/Freshness plus the refresh error recovery action.

The API does not refresh or mutate an individual Leg, accept a durable Journey ID, push partial Network Snapshot changes into an open screen, track rider progress, request background location, advance Guidance, or replan because the rider moved. Pull-to-refresh and a meaningful foreground refresh use the same full-replacement sequence.

## OpenAPI and shared fixtures

The implementation must publish one OpenAPI document and shared JSON fixtures generated or validated from it. Swift and Kotlin models are generated from, or contract-tested against, the same schema. Handwritten native types cannot narrow or reinterpret the wire contract.

Fixtures must cover at least:

- both supported Via locales and operator-text fallback;
- all departure-setting variants;
- every Travel Mode alone and in combinations, with Walking connectors;
- one, two, and three ordered Journey Options;
- Reliable and Feasible classes, labels, and ranking reasons;
- every Leg type and explicit Transfers;
- polyline6 decoding and coordinate order on both platforms;
- scheduled versus live-estimated transit times;
- fresh, degraded, unavailable, and not-applicable evidence;
- YouBike zero, unknown, one, two, and three-or-more pickup/return counts plus disabled actions;
- scoped required and optional warnings;
- every Guidance, empty, and error code in Traditional Chinese and English;
- unknown optional and required semantic codes;
- `200` result, `200` empty, `400`, `503`, and `500` outcomes;
- full refresh with selection preserved, selection lost, and refresh failure; and
- byte-equivalent ordering and semantic facts for identical request/snapshot/policy inputs.

## Validation contract

The shared contract is conformant only when repeatable checks prove that:

- MapKit and Android place search both produce the same provider-neutral Planning Request shape;
- no provider place ID, raw search text, native-framework type, raw feed status, or provider alias crosses the boundary;
- request and response data is absent from logs, traces, retained history, and shared caches;
- iOS and Android render equivalent facts, states, actions, and recovery paths from the same fixtures;
- neither client ranks Journey Options, evaluates Freshness, synthesizes YouBike counts, or interprets raw operator states;
- all Journey Options in one response use one Network Snapshot and one set of policy versions;
- scheduled, calculated, and live-estimated facts remain distinguishable;
- zero and unknown YouBike counts never collapse into one another;
- every required semantic code has supported Traditional Chinese and English client resources before release;
- an unknown required semantic code fails safely instead of being guessed;
- refresh replaces the complete result atomically and never presents retained Live Status as newly fresh; and
- breaking schema or semantic changes cannot silently reach an incompatible released client.

## Consequences

- Swift/MapKit and Kotlin clients can render the same Journey Options without sharing framework types or duplicating server rules.
- Provider adapters and identity churn remain behind Via's normalization boundary.
- Client-owned translation preserves native quality while semantic catalog tests prevent behavioral drift.
- Explicit Network Snapshot, Freshness, and actual YouBike evidence make Via's Reliable/Feasible claims auditable in ways the reviewed hosted map APIs do not.
- Full-request refresh is less incremental than Leg mutation, but it preserves one coherent result and a simple privacy boundary.
- The closest external structural precedent and the deliberate departures are recorded in [the issue #3 API research](../research/issue-3-existing-map-api-contracts.md).
