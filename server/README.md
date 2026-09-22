# Via server

The Rust/Axum server exposes Via's provider-neutral journey-planning API. TDX
is the current transit source, Valhalla is the street-routing provider, and
the runtime loads validated immutable `TransitNetwork` and `BikeShareNetwork`
artifacts.

## Run locally

The static-data and route-debug workflow is in [`docs/dev-tooling.md`](../docs/dev-tooling.md).
Copy the repository environment template, fill in TDX credentials, publish a
network artifact, and start the complete backend development stack from the
repository root:

```sh
cp .env.example .env
docker compose -f server/docker-compose.yml up --build
```

The iOS app remains a host process. The API is published at
`http://127.0.0.1:3000` for the iOS Simulator, while Valhalla is also available
at `http://127.0.0.1:8002` for host-side debugging.

The server exposes `POST /v1/journey-plans`, `GET /openapi.json`, and
`GET /health/street-routing`. Planning responses are not cached.

Configuration:

| Variable | Purpose |
| --- | --- |
| `VIA_ENV` | `development`, `staging`, or `production`. |
| `VIA_VALHALLA_URL` | Valhalla base URL. |
| `VALHALLA_TILE_URL` | OSM PBF URL used by the local scripted Valhalla container. |
| `VIA_TDX_BASE_URL` | TDX API base URL. |
| `TDX_CLIENT_ID`, `TDX_CLIENT_SECRET` | TDX OAuth credentials. |
| `VIA_TDX_SERVICE_CONDITION_PATH` | Optional service-condition path. |
| `VIA_SERVICE_CONDITION_CACHE_TTL_SECONDS` | Service-condition cache TTL; default 45 seconds. |
| `VIA_TRANSIT_NETWORK` | Validated `transit-network.json` path. |
| `VIA_BIKESHARE_NETWORK` | Validated `bikeshare-network.json` path. |
| `PORT` | Axum port; default 3000. |

## Current routing boundary

```text
PlanningRequest → RoutingService → street access / egress
→ TransitPlanner
→ DestinationReachability → TimingSnapshot → temporal search
→ BikeJourneyAugmenter → reconstruct → rank → JourneyOption[]
```

`RoutingService` owns the concrete `TransitPlanner`. The planner first finds
structurally useful boarding positions without reading time data, then loads all
request-local timing through `TimingSnapshotLoader`, and finally searches using
only the immutable network, snapshot, and static transfer index. Bus realtime
requests are deduplicated by stop and grouped by TDX city scope; temporal search
never performs network I/O.

Static `TransitNetwork.transfer_edges` contain directional transit walking
transfers. Requests select fine-grained transport systems through
`selected_modes`: `bus`, `metro`, `tra`, `thsr`, and optionally `bike_share`.
Walking remains an implicit connector, while output legs retain the coarse
`walking`, `transit`, and `bicycle` `JourneyMode` values. BikeShare augments
the direct walking baseline or a transit skeleton when selected; walking
remains the implicit connector.

The `via-route` binary is intentionally an offline transit-debugging tool and
rejects `bike_share` rather than constructing a fake BikeShare capability.
Production HTTP assembly requires the immutable BikeShare artifact and its
availability source explicitly.

The server has no database, persistence layer, identity registry, or alternate
planner. TDX adapters construct the same Via-owned typed IDs for static and
realtime data.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test --all
cargo run --bin export-openapi > /tmp/via-openapi.json
```

The checked-in Valhalla and street-router fixtures allow the contract and route
tests to run without a production graph. Real static builds still require a
reviewed TDX snapshot and a healthy Valhalla service.
