# Via server

The Rust/Axum server exposes Via's provider-neutral journey-planning API. It
loads one validated `.via-static.json` artifact, then uses TDX for realtime
transit timing and YouBike availability, and Valhalla for street routing.

## Run locally

The complete static-data and routing workflow is in
[`docs/dev-tooling.md`](../docs/dev-tooling.md). Copy the repository
environment template, add TDX credentials, and set `VIA_STATIC` to the artifact
created by `via-data build`:

```sh
cp server/.env.example server/.env
```

Start the backend development stack from the repository root, setting
`VIA_STATIC` to the container path for the built artifact:

```sh
VIA_STATIC=/app/server/data/build/20260928T043000Z/.via-static.json \
  docker compose -f server/docker-compose.yml up --build
```

The iOS app remains a host process. The API is published at
`http://127.0.0.1:3000`, and Valhalla is available at
`http://127.0.0.1:8002` for host-side debugging.

The server exposes `POST /v1/journey-plans`, `GET /openapi.json`, and
`GET /health/street-routing`. Planning responses are not cached.

Configuration:

| Variable | Purpose |
| --- | --- |
| `VIA_ENV` | `development` or `production`. |
| `VIA_STATIC` | Validated `.via-static.json` path. |
| `VIA_VALHALLA_URL` | Valhalla base URL. |
| `VALHALLA_TILE_URL` | OSM PBF URL used by the local scripted Valhalla container. |
| `VIA_TDX_BASE_URL` | TDX API base URL. |
| `TDX_CLIENT_ID`, `TDX_CLIENT_SECRET` | TDX OAuth credentials. |
| `PORT` | Axum port; default 3000. |

## Current routing boundary

```text
build_runtime → ViaRuntime
→ HTTP app / via-route → RoutingService.plan()
→ transit access / egress when transit modes are selected
→ TransitPlanner.begin_plan() → MovementSession.evaluate()
→ Phase 1: static Top-K transit corridors
→ EvaluationCorridors with Origin / BoardingPoint / Destination anchors
→ request-local snapshots for retained transit positions and selected BikeShare
→ Phase 2: fixed-corridor baselines + Walking / BikeShare surface alternatives
→ trivial Origin → Destination corridor when no candidate survives
→ JourneyReconstructor → rank → JourneyOption[]
```

`ViaRuntime` is the shared assembly used by the HTTP server and `via-route`.
`RoutingService` owns the application planning use case and discovers transit
access / egress only when a transit mode is selected. `TransitPlanner` owns
both phases and keeps its topology and transfer indexes private. Phase 1
retains one global Top-K set of structurally distinct transit corridors across
selected modes. Phase 2 converts them to `EvaluationCorridor` values with
explicit Origin, BoardingPoint, and Destination anchors. Bounded access and
egress choices stay represented on corridor edges. Empty `selected_modes`
skips transit search and timing requests; walking remains implicitly enabled.
`[bike_share]` also skips transit search and timing while loading one
BikeShare availability snapshot.

`FixedCorridorEvaluator` uses the retained baselines and a small set of
`SurfaceMovementPlanner` implementations: Walking and BikeShare. Both screen
with an optimistic lower bound before exact street routing and cache realized
routes within the request. The evaluator can choose equivalent transit
Patterns and departures, but cannot add anchors or combine corridors. If
retained transit evaluation yields no candidate, the planner evaluates a
trivial Origin-to-Destination corridor through the same evaluator. Bus
realtime requests are deduplicated by stop and grouped by TDX city scope; the
evaluator performs no TDX calls.

`JourneyReconstructor` owns place conversion, transit and BikeShare lookups,
street hydration, and journey candidate assembly. It receives the shared
request context and does not depend on the planning service. `TransitRuntime`
exposes raw immutable network access as `network()` for search, timing, and data
inspection; Health uses its focused revision and validation operations.

Static `TransitNetwork.transfer_edges` contain directional transit walking
transfers. Requests select optional non-walking systems through
`selected_modes`: `bus`, `metro`, `tra`, `thsr`, and optionally `bike_share`.
An empty list means walking alone. Walking remains an implicit capability,
while output legs retain the coarse `walking`, `transit`, and `bicycle`
`JourneyMode` values.

Both the HTTP server and `via-route` load the same `ViaStatic` artifact and use
the same runtime assembly. BikeShare can realize a movement between corridor
anchors, including the trivial fallback corridor, when station metadata and
fresh availability support it.

The server has no database, persistence layer, identity registry, or alternate
planner. TDX adapters construct the same Via-owned typed IDs for static and
realtime data.

## Verification

From the repository root:

```sh
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path server/Cargo.toml --all-targets --all-features
cargo run --manifest-path server/Cargo.toml --bin export-openapi > /tmp/via-openapi.json
```
