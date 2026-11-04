# Via server

Configuration is centralized in [config/via.toml](config/via.toml). See the [configuration reference](docs/configuration.md) for precedence, validation, provider limits, and artifact rebuild requirements.

The Rust/Axum server exposes Via's provider-neutral journey-planning API. It
loads one validated `.via-static.json` artifact, then uses TDX for realtime
transit timing and YouBike availability, and Valhalla for street routing.

## Run locally

Copy the environment template and add TDX credentials:


```sh
cp server/.env.example server/.env
```

From the repository root, fetch source data and build an artifact with Valhalla
running. `fetch` downloads the TDX GTFS feed unless `--gtfs-file` is given, and
points `server/data/fetch/latest` at the new snapshot; `build` points
`server/data/build/.via-static-latest.json` at the new artifact. `build` reads
the latest snapshot and `validate`/`inspect` read the latest artifact unless
`--input` or `--static` is given:

```sh
cargo run --manifest-path server/Cargo.toml --bin via-data -- fetch
cargo run --manifest-path server/Cargo.toml --bin via-data -- build
cargo run --manifest-path server/Cargo.toml --bin via-data -- validate
```

`fetch` also retrieves TDX station catalogs and needs credentials; `build` reads
local inputs and requires Valhalla. See [development, operations and testing](https://app.notion.com/p/408aefae2f7481cfb632eb59c2a32b95)
for route debugging, tracing, metrics and benchmark prerequisites.

Start the backend development stack from the repository root, setting
`VIA_STATIC` to the container path for the built artifact:

```sh
VIA_STATIC=/app/server/data/build/20260928T043000Z/.via-static.json \
  docker compose -f server/docker-compose.yml up --build
```

The iOS app remains a host process. The API is published at
`http://127.0.0.1:3000`, and Valhalla is available at
`http://127.0.0.1:8002` for host-side debugging. To reach the API from a phone on
the same tailnet, keep it on loopback and publish it over HTTPS with
`tailscale serve --bg --https=443 http://127.0.0.1:3000` (Serve and HTTPS
certificates must be enabled for the tailnet); the API is then at
`https://<machine>.<tailnet>.ts.net`. Alternatively, set `VIA_BIND_ADDRESS` to an
address the phone can reach, such as the Mac's Tailscale IP (`tailscale ip -4`).

The server exposes `POST /v1/journey-plans`, `GET /v1/nearby-stops`,
`GET /openapi.json`, and `GET /health/street-routing`. Responses are not cached.

`GET /v1/nearby-stops?latitude=…&longitude=…&radius_metres=…` lists transit stops
from the static artifact (same-name entries within 50 m are merged, with their
modes and route labels) and YouBike stations with TDX availability, nearest first.
The radius defaults to 500 m and is capped at 2 km.

`GET /metrics` serves Prometheus telemetry. See the
[metric inventory and ownership](https://app.notion.com/p/408aefae2f7481cfb632eb59c2a32b95) for names and completion semantics.

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

`ViaRuntime` assembles the HTTP server and `via-route`. Both entry points convert
validated API requests into domain-owned `PlanningInput`. `RoutingService`
coordinates admission, deadlines, planning, reconstruction and final ranking.
`MultimodalPlanner` owns transit access/egress discovery, static corridor
search and request-local snapshots, and `CorridorEvaluator` returns journeys,
statistics and observed failures or a single routing error. It evaluates the direct walking corridor and retained transit
corridors in one pass, starting direct walking first while allowing bounded overlap.
Walking is always available; `selected_modes` lists optional transit and BikeShare
modes. `JourneyReconstructor::reconstruct_all` hydrates geometry concurrently with a
request-local Route cache, restores candidate ordering and collects failures
before final ranking.

Rust callers use the exports at `routing` rather than implementation submodules.
Provider implementations satisfy domain-facing interfaces; routing does not
depend on API request types or concrete providers. HTTP schemas, CLI inputs,
matching keys and result-local identifiers remain unchanged by this organization.

See [CONTEXT.md](../CONTEXT.md) for the current architecture and
[configuration.md](docs/configuration.md) for runtime settings.

## Verification

From the repository root:

```sh
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --locked --all-targets --all-features -- -D warnings
cargo test --manifest-path server/Cargo.toml --locked --all-targets --all-features
cargo run --manifest-path server/Cargo.toml --bin export-openapi > /tmp/via-openapi.json
```

For a fast unit-test pass, run `cargo test --manifest-path server/Cargo.toml --locked --lib`.
The full command above runs the offline correctness suite; ignored measurements
are excluded. There are currently no separate live-service correctness tests.
For the optional Valhalla pilot and other manual measurements, prerequisites and
individual commands are documented in the linked Notion testing guide. Run the
offline routing measurement with:

```sh
cargo test --manifest-path server/Cargo.toml --locked --release --test performance -- --ignored --nocapture
```
