# Via server

Configuration is centralized in [config/via.toml](config/via.toml). See the [configuration reference](docs/configuration.md) for precedence, validation, provider limits, and artifact rebuild requirements.

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

`GET /metrics` serves Prometheus telemetry. See the
[metric inventory and ownership](../docs/observability.md) for names and completion semantics.

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

`ViaRuntime` assembles the HTTP server and `via-route`. `RoutingService` discovers
transit access and egress when needed, then `MultimodalPlanner` prepares static
transit corridors and request-local snapshots. `PlanningSession.evaluate()`
evaluates the direct walking corridor and retained transit corridors in one pass.
Walking is always available; `selected_modes` lists optional transit and BikeShare
modes. Reconstruction hydrates and ranks the resulting journeys.

See [CONTEXT.md](../CONTEXT.md) for the current architecture and
[configuration.md](docs/configuration.md) for runtime settings.

## Verification

From the repository root:

```sh
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path server/Cargo.toml --all-targets --all-features
cargo run --manifest-path server/Cargo.toml --bin export-openapi > /tmp/via-openapi.json
```
