# Via development data and routing tools

Static data has two stages. `via-data fetch` saves immutable external source
snapshots. `via-data build` turns one snapshot into a validated
`.via-static.json` using the local Valhalla service. TDX realtime supplies
transit timing and YouBike availability at request time.

## Fetch source data

From the repository root, set TDX credentials in `.env` or the process
environment, then run:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- fetch \
  --gtfs-file /workspaces/Via/server/data/dev/gtfs.zip
```

The local GTFS override is copied byte-for-byte and makes no GTFS HTTP request.
The fetch still downloads current Taipei and New Taipei YouBike station
metadata from TDX. Without `--gtfs-file`, the command downloads the TDX GTFS
Static ZIP. Both sources use the shared TDX OAuth client.

Each successful run creates `server/data/fetch/{YYYYMMDDTHHMMSSZ}/` with
`gtfs.zip`, the raw first response pages
`bike-stations-taipei.json` and `bike-stations-new-taipei.json`, and
`manifest.json`. Additional raw pages use
`bike-stations-taipei-page-NNNN.json` and
`bike-stations-new-taipei-page-NNNN.json`.
The manifest records each page's source identifier, source URL without
credentials, byte size, and SHA-256 hash. The directory is published only
after all required sources pass basic validation.

## Build and validate

Use the path printed by `via-data fetch` as the build input:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- build \
  --input server/data/fetch/{fetch_datetime}
```

The builder parses the original GTFS ZIP directly in Rust, builds transit
structures and TDX provider bindings, normalizes station metadata in memory,
and uses Valhalla to generate directional transit walking transfers and
transit-to-YouBike station access links. Bus stop-time rows with incomplete
intermediate times are not turned into scheduled Trips. If terminal anchors
provide a full runtime, the builder distributes it across adjacent stops using
shape distance, then geographic distance. These derived segment costs support
Phase 1 and are not timetable data.

Each build creates one runtime static artifact at
`server/data/build/{YYYYMMDDTHHMMSSZ}/.via-static.json`:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- validate \
  --static server/data/build/{build_datetime}/.via-static.json

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect stats \
  --static server/data/build/{build_datetime}/.via-static.json
```

Other inspect operations accept the same `--static` path:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect stop \
  --static server/data/build/{build_datetime}/.via-static.json \
  --query "台北車站"

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect pattern \
  --static server/data/build/{build_datetime}/.via-static.json \
  --query "307"

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect nearby-stops \
  --static server/data/build/{build_datetime}/.via-static.json \
  --lat 25.0339 --lon 121.5645 --limit 20
```

## Live route debugging

`via-route` uses the same runtime assembly as the HTTP server, including live
TDX timing, Valhalla, and BikeShare availability. It accepts the same immutable
static artifact:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-route -- \
  --static server/data/build/{build_datetime}/.via-static.json \
  --from-lat 25.0478 --from-lon 121.5170 \
  --to-lat 25.0339 --to-lon 121.5645 \
  --depart-now \
  --modes bus,metro,bike_share \
  --trace
```

The CLI also accepts `--depart-at <RFC3339>`, `--arrive-by <RFC3339>`, and
`--json`. `--modes` accepts comma-separated `bus`, `metro`, `tra`, `thsr`, and
`bike_share`. BikeShare can shortcut forward between anchors in a retained
corridor or replace a direct walking journey when station metadata, street
routes, and fresh availability support it. It does not add transit topology.

## Run the server

Set `VIA_ENV` to `development` or `production` and `VIA_STATIC` to
the same artifact used by `via-route`. `PORT` is optional and defaults to
`3000`:

```bash
VIA_ENV=development \
VIA_STATIC=server/data/build/{build_datetime}/.via-static.json \
  cargo run --manifest-path server/Cargo.toml --bin via-server

VIA_ENV=development \
VIA_STATIC=/app/server/data/build/20260928T043000Z/.via-static.json \
  docker compose -f server/docker-compose.yml up --build
```

Replace the example build timestamp with the directory created on your run.
The server and live `via-route` command read TDX credentials, `VIA_TDX_BASE_URL`,
and `VIA_VALHALLA_URL` at startup. TDX defaults to its HTTPS API URL; Valhalla
defaults to the local development URL, so production must set an HTTPS
`VIA_VALHALLA_URL`. Production validates HTTPS for both upstreams. The live
route command defaults a missing `VIA_ENV` to development and rejects an
unsupported value. `via-data fetch` reads the TDX URL and credentials;
`via-data build` reads the Valhalla URL and uses its 15-second build request
timeout. Local `via-route` flags override its routing limits and Valhalla
timeout. The included Compose stack forwards `VIA_ENV` and
`VIA_VALHALLA_URL`; its default Valhalla endpoint uses HTTP for local
development, so the stack's default settings are not suitable for production.

The server exposes `POST /v1/journey-plans`. The request API and journey
response shape are unchanged by the static-data workflow.

`TransferShortcutIndex` is projected from validated directional
`TransitNetwork.transfer_edges`. GTFS `transfers.txt` is ignored.

ViaStatic schema version 2 requires measured `distance_metres` on static walking
transfer edges. Rebuild version 1 artifacts with `via-data build`; missing
transfer distances are not inferred from walking speed.
