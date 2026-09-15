# Via development data and routing tools

The development data flow is deliberately offline-first:

```text
TDX → via-data fetch → immutable raw snapshot → via-data build
  → validated TransitNetwork + transfer shortcuts → server / via-route
```

The first supported scope is `taipei-new-taipei`. Static artifacts are not
downloaded by the production server.

## Credentials

From the repository root:

```bash
cp .env.example .env
```

Fill in:

```env
TDX_CLIENT_ID=your_client_id
TDX_CLIENT_SECRET=your_client_secret
```

`dotenvy` is initialized by `via-data`, `via-route`, and the server. Shell
environment values retain dotenvy's normal precedence. Credentials are loaded
through the shared `TdxCredentials` and `TdxClient` types; OAuth tokens are
cached only in the current process and never written to snapshots, artifacts,
manifests, or traces.

`via-data fetch` and `via-route --realtime live` require these credentials.
Build, validate, inspect, and `via-route --realtime fixture` work without a
`.env` file or network access.

## CLI hierarchy

```text
via-data
├── fetch
├── build
├── validate
└── inspect
    ├── stop
    ├── service
    ├── nearby-stops
    ├── bike-near
    └── stats

via-route
└── --realtime fixture|live
```

## Fetch and build

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- fetch \
  --scope taipei-new-taipei \
  --date 2026-09-15 \
  --output ./data/dev/taipei

cargo run --manifest-path server/Cargo.toml --bin via-data -- build \
  --input ./data/dev/taipei/raw/2026-09-15 \
  --output ./data/dev/taipei

cargo run --manifest-path server/Cargo.toml --bin via-data -- validate \
  --artifact ./data/dev/taipei/transit-network.json
```

Fetch preserves provider response bodies under `raw/<date>/`, writes source
hashes and retrieval metadata to `raw/<date>/manifest.json`, uses bounded
concurrency and timeouts, and fails without writing an incomplete manifest.

The build output is:

```text
transit-network.json
transfer-shortcuts.json
manifest.json
```

The network is canonicalized and validated before it is published. Revisions
are SHA-256 hashes of canonical normalized content, not wall-clock identities.
The manifest records source hashes, entity counts, revisions, and transfer
preprocessing bounds. `built_at` is diagnostic metadata and may vary.

The current static source families are TDX Bus Route/Stop/StopOfRoute/Schedule for
Taipei and New Taipei, TDX YouBike Station for both cities, TDX Taipei Metro
station/line/StationTimeTable/S2STravelTime/Frequency data, and TDX TRA/THSR
station and timetable data.
Provider payload parsing remains inside `via_server::data`; routing consumes
only the provider-neutral artifact.

## Inspect

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect stop \
  --artifact ./data/dev/taipei/transit-network.json \
  --query "台北101"

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect service \
  --artifact ./data/dev/taipei/transit-network.json \
  --query "307"

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect nearby-stops \
  --artifact ./data/dev/taipei/transit-network.json \
  --lat 25.0339 --lon 121.5645 --limit 20

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect bike-near \
  --artifact ./data/dev/taipei/transit-network.json \
  --lat 25.0339 --lon 121.5645 --limit 20

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect stats \
  --artifact ./data/dev/taipei/transit-network.json
```

Nearby queries use the same `TransitNetwork` spatial index as production
routing. Bike inspection is static only and never requests availability.

## Route debugging

The fixture route uses the actual `RoutingService → RoutingAlgorithm →
TripBasedRouter → reconstruct → rank` path with deterministic fixture street
and realtime providers:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-route -- \
  --network ./data/dev/taipei/transit-network.json \
  --from-lat 25.0478 --from-lon 121.5170 \
  --to-lat 25.0339 --to-lon 121.5645 \
  --depart-at 2026-09-15T07:55:00+08:00 \
  --modes transit,bike_share \
  --realtime fixture \
  --trace
```

For an integration smoke test against TDX realtime, keep the same static
artifact and switch only the realtime provider:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-route -- \
  --network ./data/dev/taipei/transit-network.json \
  --from-lat 25.0478 --from-lon 121.5170 \
  --to-lat 25.0339 --to-lon 121.5645 \
  --depart-now \
  --modes transit,bike_share \
  --realtime live \
  --trace
```

The CLI also accepts `--depart-at <RFC3339>`, `--arrive-by <RFC3339>`, and
`--json`. It accepts only the product-level `transit` and `bike_share` modes.
It never accepts credential flags.

Recommended smoke routes are Taipei Main Station → Taipei 101, Banqiao →
Taipei 101 for transfer structure, and an origin/destination away from MRT
for a Bus + Metro or YouBike + Metro check. Live arrival times are not brittle
test assertions; verify route existence, leg structure, transfers, and
positive realistic durations.

The initial transfer output is a bounded, generic `TransferShortcutIndex`.
It preserves multiple alternatives and stores no live BikeShare inventory. By
default, `via-data build` publishes a reviewed raw transfer artifact when one is
present, or only the explicit static walking edges otherwise. Street-backed
preprocessing is explicit:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- build \
  --input ./data/dev/taipei/raw/2026-09-15 \
  --output ./data/dev/taipei \
  --preprocess-transfers --street-router valhalla
```

Valhalla preprocessing requires `VIA_VALHALLA_GRAPH_REVISION` and
`VIA_VALHALLA_TILESET_LAST_MODIFIED`; `--street-router straight-line` is
available for deterministic development fixtures. Exact ULTRA preprocessing
remains deferred. The server and `via-route` load the sibling
`transfer-shortcuts.json` artifact (or `VIA_TRANSFER_SHORTCUTS` for the server)
when it exists; their bounded builder remains only as a fallback when no
artifact is available. `TripBasedIndex` is derived at server or CLI startup
from the validated network rather than serialized by this first tooling
version.
