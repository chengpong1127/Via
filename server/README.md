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
PlanningRequest → RoutingService → street access / egress
→ TransitPlanner
→ Phase 1: static Top-K structural corridors
→ request-local TimingSnapshot + optional BikeAvailabilitySnapshot
→ Phase 2: fixed corridor evaluation (Transit / Walk / YouBike)
→ reconstruct → rank → JourneyOption[]
```

`RoutingService` owns the concrete `TransitPlanner`. The planner first retains
one global Top-K set of structurally distinct corridors across selected modes.
It loads one request-local timing snapshot for their positions and optionally
loads one BikeShare availability snapshot. The Phase 2 evaluator walks each
retained corridor in order and uses only immutable networks and those
snapshots. It may choose an equivalent transit Pattern and departure, or
realize a fixed surface transfer with walking or YouBike. It cannot add a
transfer anchor or combine different corridors. Bus realtime requests are
deduplicated by stop and grouped by TDX city scope; the evaluator performs no
network I/O.

Static `TransitNetwork.transfer_edges` contain directional transit walking
transfers. Requests select fine-grained transport systems through
`selected_modes`: `bus`, `metro`, `tra`, `thsr`, and optionally `bike_share`.
Walking remains an implicit connector, while output legs retain the coarse
`walking`, `transit`, and `bicycle` `JourneyMode` values. Direct walking remains
walking-only in this version.

Both the HTTP server and `via-route` load the same `ViaStatic` artifact and use
the same production runtime assembly. BikeShare can realize a retained surface
transfer when station metadata and fresh availability support it; it does not
create direct BikeShare-only routes.

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
