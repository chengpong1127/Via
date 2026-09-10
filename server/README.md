# Via server

The Rust/Axum server owns Via's provider-neutral Journey Planning contract and
street-routing boundary. A Planning Request pins one immutable Network Snapshot,
uses no government feed on the request path, and returns a complete result with
`Cache-Control: no-store`.

Implemented capabilities:

- `POST /v1/journey-plans` with strict provider-neutral request validation,
  redacted errors, full-result refresh semantics, and polyline6 Walking results;
- deterministic Live Status eligibility, Reliable/Feasible classification,
  Transfer and YouBike boundaries, dominance, ranking, diversity, labels, and
  replay behavior;
- stable typed network identities, reviewed provider aliases, explicit TDX N1
  status normalization, Service Day handling, and separate YouBike timestamps;
- PostgreSQL-authoritative immutable Network Snapshots, warm immutable caching,
  leased and idempotent publication, optimistic publication-version checks,
  partial feed evidence, and retention cleanup;
- authenticated `POST /internal/v1/refresh` with at most four concurrent feed
  adapters and atomic snapshot publication;
- `GET /openapi.json` plus shared request, result, empty, and error fixtures;
- coordinate correlation;
- one-to-many and many-to-one Walking/YouBike matrices;
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
docker compose up -d valhalla
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
| `VIA_ENV` | Required deployment environment: `development`, `staging`, or `production`; production rejects non-HTTPS routing and feed URLs. |
| `VIA_VALHALLA_URL` | Valhalla base URL; defaults to `http://127.0.0.1:8002/`. |
| `DATABASE_URL` | Pooled PostgreSQL endpoint; required. |
| `VIA_DATABASE_MAX_CONNECTIONS` | Small per-instance pool size; defaults to `4`. |
| `VIA_STATIC_NETWORK_REVISION` | Immutable normalized network revision; required on first bootstrap. |
| `VIA_SCHEDULE_REVISION` | Immutable schedule revision; required on first bootstrap. |
| `VIA_REFRESH_TOKEN` | Bearer token used only by the external scheduler; required. |
| `VIA_TDX_BEARER_TOKEN` | TDX access token held only by feed adapters; required. |
| `VIA_TDX_FEEDS` | JSON array of reviewed TDX feeds with `capability`, `url`, `family`, and `authority_namespace`; required and non-empty. |
| `VIA_TRANSIT_NETWORK` | Path to the immutable normalized Bus/Metro/YouBike network and timetable JSON artifact; required. |
| `PORT` | Axum port; defaults to `3000`. |

Database migrations run at startup. The first startup publishes one coherent
bootstrap snapshot. Later refreshes acquire a 55-second PostgreSQL lease and
publish only if their expected publication version still matches. Snapshot and
observation retention cleanup runs in the same successful publication cycle.

TDX feed adapters enforce an eight-second deadline and 8 MiB response bound,
translate rate limits and malformed payloads into explicit feed evidence, and
evaluate Freshness from the provider publication timestamp rather than a station's
last-change timestamp. Planning Requests never receive feed credentials and never
invoke adapters.

## Current planner boundary

The request path pins one Network Snapshot, then orchestrates direct Walking,
bounded timetable Bus/Metro rounds, and inventory-gated YouBike access/bicycle/
egress candidates. Street access, egress, bicycle, matrix, and path calculations
stay behind the Valhalla provider boundary; timetable lookup uses the immutable
`TransitNetwork` artifact. The checked-in tests use injected fixtures, while a
production deployment must publish a reviewed artifact at `VIA_TRANSIT_NETWORK`.

## Tests

```sh
cargo test --test provider_contract
cargo test --test valhalla_contract
cargo test --test planning_contract
cargo test --test ranking_contract
cargo test --test refresh_contract
cargo test --test journey_endpoint
cargo test --test transit_contract
cargo test
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
