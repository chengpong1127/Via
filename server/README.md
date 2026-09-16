# Via server

The Rust/Axum server owns Via's provider-neutral Journey Planning contract and
street-routing boundary. TDX is currently the authoritative source for Taiwan
transit static and realtime data. A Planning Request reads one immutable
`Arc<TransitNetwork>` for static schedules, then demand-fetches only the
realtime facts needed by its scheduled candidates through an in-process async
cache. Results use `Cache-Control: no-store`.

Implemented capabilities:

- `POST /v1/journey-plans` with strict provider-neutral request validation,
  redacted errors, complete result replacement, and polyline6 Walking results;
- deterministic Live Status eligibility, Reliable/Feasible classification,
  Transfer and bike-share boundaries, dominance, ranking, diversity, labels, and
  replay behavior;
- strongly typed TDX-derived network IDs, explicit TDX N1 status normalization,
  Service Day handling, and separate bike-share timestamps;
- Moka-backed demand-driven realtime caching with async request coalescing;
- scoped TDX arrival and BikeShare queries with provider-neutral normalization;
- generated OpenAPI at `GET /openapi.json`, coupled to Axum route registration,
  plus shared request, result, empty, and error fixtures;
- one-to-many and many-to-one Walking/bike-share matrices;
- provider-neutral route geometry and best-effort maneuver facts;
- Valhalla-owned graph labeling and tileset validation;
- normalized provider errors, strict timeout/concurrency/provider/body bounds;
- structured OSM attribution; and
- Operational Events containing only provider name, Valhalla revision label, latency,
  outcome, and anonymous counts.

## Run locally

Development data and route debugging commands are documented in
[`docs/dev-tooling.md`](../docs/dev-tooling.md). In brief, copy the repository
example environment file with `cp .env.example .env`; `via-data fetch` and
`via-route --realtime live` use the shared `TDX_CLIENT_ID`/
`TDX_CLIENT_SECRET` OAuth configuration, while build, validation, inspection,
and fixture routing remain fully offline.

Start the pinned Valhalla 3.8.3 image, then build the checked-in deterministic
street fixture:

```sh
docker compose up -d valhalla
./scripts/wait-for-valhalla.sh
```

Record the `tileset_last_modified` integer from that status response. The server
requires it so a configured graph revision cannot silently name different tiles.

Then start Axum:

```sh
cargo run
```

At startup, the server loads configuration with dotenvy. Variables already set
in the process environment take precedence.

The server fails startup until Valhalla is healthy. Its operator endpoint is
`GET /health/street-routing`; responses use `Cache-Control: no-store` and report
only the provider-neutral health state and provider name.

Configuration:

| Variable | Meaning |
| --- | --- |
| `VIA_ENV` | Required deployment environment: `development`, `staging`, or `production`; production rejects non-HTTPS routing and TDX URLs. |
| `VIA_VALHALLA_URL` | Valhalla base URL; defaults to `http://127.0.0.1:8002/`. |
| `VIA_VALHALLA_GRAPH_REVISION` | Valhalla-owned logical revision label for the loaded graph artifact; required. |
| `VIA_VALHALLA_TILESET_LAST_MODIFIED` | Valhalla `/status` tileset timestamp expected at startup and during health checks; required. |
| `VIA_TDX_BASE_URL` | TDX API base URL used for scoped realtime requests; required. |
| `TDX_CLIENT_ID` | Shared TDX OAuth client ID used by the realtime and static-data clients. |
| `TDX_CLIENT_SECRET` | Shared TDX OAuth client secret used with `TDX_CLIENT_ID`; never passed as a CLI flag or written to artifacts. |
| `VIA_TDX_SERVICE_CONDITION_PATH` | Optional scoped TDX service-condition endpoint path. |
| `VIA_REALTIME_CACHE_TTL_SECONDS` | In-process realtime cache TTL; defaults to 45 seconds. |
| `VIA_TRANSIT_NETWORK` | Path to the immutable normalized transit and bike-share network and timetable JSON artifact; required. |
| `PORT` | Axum port; defaults to `3000`. |

Static network and schedule revisions come from the validated immutable
`VIA_TRANSIT_NETWORK` artifact. The server has no database runtime dependency.

The TDX adapter enforces an eight-second deadline and 8 MiB response bound,
translates rate limits and malformed payloads into explicit realtime failures, and
evaluates Freshness from the provider publication timestamp rather than a station's
last-change timestamp. Planning Requests never receive feed credentials.

## Current identity model

Via owns the domain ID types, while TDX currently owns the underlying transit
identity values. TDX `StopUID`, `SubRouteUID`, and `StationUID` become the
corresponding `BoardingPointId`, `TransitServiceId`, and `BikeShareStationId`
through pure constructors. Static artifact generation and realtime normalization
must use the same constructors, so matching entities have equal typed IDs.

TDX UIDs include the authority code required by the current feeds, so no extra
authority namespacing or identity registry is required. The current service
artifact may use `SubRouteUID` directly; when a static artifact models each
direction separately, it uses the deterministic directional constructor in the
TDX adapter and realtime queries project that ID back to its `SubRouteUID`.

The runtime dependency graph is:

```text
Via Server → immutable TransitNetwork artifact
           → TDX realtime API
           → Valhalla
```

## Current routing boundary

Walking is always available for direct journeys and transit/BikeShare access and
egress; direct BikeShare is also a separate candidate, and clients select only
the high-level `transit` and `bike_share` categories. The request path is:

```text
PlanningRequest → RoutingService → street access/egress
→ RoutingAlgorithm → TripBasedRouter → reconstruct → rank → JourneyOption[]
```

`RoutingAlgorithm` exposes only algorithm-neutral access/egress facts and
`JourneyCandidate` results. `TripBasedRouter` uses a private `TripBasedIndex`,
resolves complete `EffectiveTrip` instances lazily through `DepartureResolver`,
and expands `TransferShortcutIndex` alternatives. Street access, egress,
bicycle, matrix, and path calculations stay behind the Valhalla provider
boundary; timetable lookup uses the immutable `TransitNetwork` artifact. The
checked-in tests use injected fixtures, while a production deployment must
publish a reviewed artifact at `VIA_TRANSIT_NETWORK`.

Transit timing is provider-neutral across Bus, Metro, conventional rail, and
high-speed rail. A `DepartureResolver` receives only a service, boarding point,
and ready time, then returns normalized `EffectiveTrip` instances with complete
stop timing, timing basis, and optional evidence. Routing and Pareto search do
not choose timing sources by `TransitMode`.

Walking and YouBike are transfer semantics, not transit departures. A
`TransferShortcut` retains its decomposed walk/bike path, while runtime
availability is fetched only for stations referenced by explored shortcuts.
The shortcut builder is ULTRA-compatible in representation, but exact ULTRA
offline preprocessing is intentionally deferred until the required static
street preprocessing artifact exists.

The currently wired TDX timing path is Bus `EstimatedTimeOfArrival` when the
service metadata selects `bus_n1`, with the static timetable as the deliberate
fallback for missing or unsupported timing sources and failed Bus realtime.
The same adapter boundary is ready for TRA, THSR, and Metro
live-board/timetable/frequency adapters; those live endpoints are not claimed
as implemented until their payload contracts are available.

## Tests

```sh
cargo test --test provider_contract
cargo test --test valhalla_contract
cargo test --test planning_contract
cargo test --test ranking_contract
cargo test --test realtime_contract
cargo test --test journey_endpoint
cargo test --test transit_contract
cargo test
```

For external tooling, the generated document can also be exported without
adding a checked-in artifact:

```sh
cargo run --bin export-openapi > /tmp/via-openapi.json
```

The Valhalla contract tests use an isolated local HTTP fixture and do not need
Docker. The deterministic fake is the default seam for Journey Planning tests.

## Graph publication

Graph builds are offline publication work. The fixture checksum, Valhalla image,
and Valhalla graph revision are pinned. A mounted volume with a different
tileset timestamp is rejected rather than rebuilt in place. Publish a new graph
to a new volume, validate `/status`, then direct new Planning Requests to the
new provider instance. Rollback selects the prior healthy volume.

The checked-in fixture is intentionally tiny. Taipei–New Taipei production use
still requires a separately pinned OSM extract, the accepted route-quality
corpus, attribution/legal review, and the p50/p95/p99 adoption benchmark from
issue #13.
