# Via server

The Rust/Axum server exposes Via's provider-neutral journey-planning API. TDX
is the current transit source, Valhalla is the street-routing provider, and
the runtime loads validated immutable `TransitNetwork` and `BikeShareNetwork`
artifacts.

## Run locally

The static-data and route-debug workflow is in [`docs/dev-tooling.md`](../docs/dev-tooling.md).
Copy the repository environment template, fill in TDX credentials, publish a
network artifact, and start Valhalla before starting Axum:

```sh
cp .env.example .env
docker compose up -d valhalla
./scripts/wait-for-valhalla.sh
cargo run
```

The server exposes `POST /v1/journey-plans`, `GET /openapi.json`, and
`GET /health/street-routing`. Planning responses are not cached.

Configuration:

| Variable | Purpose |
| --- | --- |
| `VIA_ENV` | `development`, `staging`, or `production`. |
| `VIA_VALHALLA_URL` | Valhalla base URL. |
| `VIA_VALHALLA_GRAPH_REVISION` | Revision label for the loaded graph. |
| `VIA_VALHALLA_TILESET_LAST_MODIFIED` | Expected Valhalla tileset timestamp. |
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
→ RoutingAlgorithm → TripBasedRouter → reconstruct → rank → JourneyOption[]
```

`RoutingAlgorithm`, `StreetRouter`, and `DepartureResolver` are the intentional
provider-neutral seams. `TripBasedRouter` consumes realtime-first `EffectiveTrip`
values and static walking `TransferShortcutIndex` alternatives.

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
reviewed TDX snapshot and a healthy Valhalla graph.
