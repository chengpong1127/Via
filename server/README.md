# Via server

The Rust/Axum server owns Via's provider-neutral Journey Planning contract and
street-routing boundary. A Planning Request reads one immutable `Arc<TransitNetwork>`
for static schedules, then demand-fetches only the realtime facts needed by its
scheduled candidates through an in-process async cache. Results use
`Cache-Control: no-store`.

Implemented capabilities:

- `POST /v1/journey-plans` with strict provider-neutral request validation,
  redacted errors, complete result replacement, and polyline6 Walking results;
- deterministic Live Status eligibility, Reliable/Feasible classification,
  Transfer and bike-share boundaries, dominance, ranking, diversity, labels, and
  replay behavior;
- stable typed network identities, reviewed provider aliases, explicit TDX N1
  status normalization, Service Day handling, and separate bike-share timestamps;
- Moka-backed demand-driven realtime caching with async request coalescing;
- scoped TDX arrival and BikeShare queries with provider-neutral normalization;
- PostgreSQL-backed, versioned provider aliases without persisted Live Status;
- generated OpenAPI at `GET /openapi.json`, coupled to Axum route registration,
  plus shared request, result, empty, and error fixtures;
- coordinate correlation;
- one-to-many and many-to-one Walking/bike-share matrices;
- detailed geometry and semantic maneuver facts;
- immutable graph-revision pinning and startup capability checks;
- normalized provider errors, strict timeout/concurrency/location/body bounds;
- structured OSM attribution; and
- Operational Events containing only provider category, revision, latency,
  outcome, and anonymous counts.

## Run locally

Start PostgreSQL plus the pinned Valhalla 3.8.3 image, then build the checked-in
deterministic street fixture:

```sh
docker compose up -d postgres valhalla
./scripts/wait-for-valhalla.sh
```

Record the `tileset_last_modified` integer from that status response. The server
requires it so a configured graph revision cannot silently name different tiles.

Then start Axum:

```sh
cargo run
```

At startup, the server loads configuration from `server/.env`. Variables already
set in the process environment take precedence.

The server fails startup until Valhalla is healthy. Its operator endpoint is
`GET /health/street-routing`; responses use `Cache-Control: no-store` and report
both the current and startup-pinned graph revision.

Configuration:

| Variable | Meaning |
| --- | --- |
| `VIA_ENV` | Required deployment environment: `development`, `staging`, or `production`; production rejects non-HTTPS routing and TDX URLs. |
| `VIA_VALHALLA_URL` | Valhalla base URL; defaults to `http://127.0.0.1:8002/`. |
| `DATABASE_URL` | Pooled PostgreSQL endpoint; required. |
| `VIA_DATABASE_MAX_CONNECTIONS` | Small per-instance pool size; defaults to `4`. |
| `VIA_TDX_BASE_URL` | TDX API base URL used for scoped realtime requests; required. |
| `VIA_TDX_AUTHORITY_NAMESPACE` | Reviewed TDX authority namespace used for alias resolution; required. |
| `VIA_TDX_BEARER_TOKEN` | TDX access token held only by the realtime adapter; required. |
| `VIA_TDX_SERVICE_CONDITION_PATH` | Optional scoped TDX service-condition endpoint path. |
| `VIA_REALTIME_CACHE_TTL_SECONDS` | In-process realtime cache TTL; defaults to 45 seconds. |
| `VIA_TRANSIT_NETWORK` | Path to the immutable normalized transit and bike-share network and timetable JSON artifact; required. |
| `PORT` | Axum port; defaults to `3000`. |

Database migrations run at startup and maintain only the versioned provider-alias
registry. Static network and schedule revisions come from the validated immutable
`VIA_TRANSIT_NETWORK` artifact.

The TDX adapter enforces an eight-second deadline and 8 MiB response bound,
translates rate limits and malformed payloads into explicit realtime failures, and
evaluates Freshness from the provider publication timestamp rather than a station's
last-change timestamp. Planning Requests never receive feed credentials.

## Current routing boundary

Walking is always available for direct journeys and transit/BikeShare access and
egress; clients select only the high-level `transit` and `bike_share`
categories. The request path is:

```text
PlanningRequest → RoutingService → discover → RouteGraph → enrich → bike
→ Pareto search → reconstruct → rank → JourneyOption[]
```

`RouteGraph` is the one canonical routing representation. Street access,
egress, bicycle, matrix, and path calculations stay behind the Valhalla
provider boundary; timetable lookup uses the immutable `TransitNetwork`
artifact. After candidate discovery, `RealtimeContext` freezes the required
values for the rest of the request. The checked-in tests use injected fixtures,
while a production deployment must publish a reviewed artifact at
`VIA_TRANSIT_NETWORK`.

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
and Via graph revision are pinned. A mounted volume with a different revision is
rejected rather than rebuilt in place. Publish a new graph to a new volume,
validate `/status`, then direct new Planning Requests to the new provider
instance. Existing `StreetRoutingSession` values remain pinned to the old
revision. Rollback selects the prior healthy volume.

The checked-in fixture is intentionally tiny. Taipei–New Taipei production use
still requires a separately pinned OSM extract, the accepted route-quality
corpus, attribution/legal review, and the p50/p95/p99 adoption benchmark from
issue #13.
