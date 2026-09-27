# Via development data and routing tools

Static transit data is built from a local GTFS archive. Request-time TDX data
supplies transit timing, service conditions, and YouBike availability.

```text
local GTFS → via-data build → TransitNetwork + optional station catalog
TDX realtime → one request-local snapshot → fixed corridor evaluation
```

`via-data` does not download or refresh GTFS or TDX files. The development
archive is `server/data/dev/gtfs.zip`.

For a local GTFS archive with noncanonical `direction_id` values or minute-only
stop times, `server/tools/prepare_gtfs.py` can normalize those fields without
fetching any data:

```bash
python3 server/tools/prepare_gtfs.py \
  --input ./server/data/dev/gtfs.zip \
  --output ./server/data/dev/prepared/gtfs.zip
```

## Build and validate

From the repository root:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- build \
  --input ./server/data/dev/gtfs.zip \
  --output ./server/data/dev/normalized

cargo run --manifest-path server/Cargo.toml --bin via-data -- validate \
  --artifact ./server/data/dev/normalized/transit-network.json
```

The builder reads `stops.txt`, `routes.txt`, `trips.txt`, `stop_times.txt`,
`shapes.txt`, `frequencies.txt`, `calendar.txt`, and `calendar_dates.txt` when
present. It ignores `transfers.txt`. Bus stop-time rows with incomplete
intermediate times are not turned into scheduled Trips. If terminal anchors
provide a full runtime, preprocessing distributes it across adjacent stops
using shape distance, then geographic distance. These derived segment costs
rank Phase 1 corridors; they are not timetable data.

The output contains `transit-network.json`, `provider-bindings.json`,
`bikeshare-network.json`, and `manifest.json`. Without station inputs, the
BikeShare artifact has no stations. To include static YouBike station metadata,
provide local TDX Taipei and New Taipei `Bike/Station/City` JSON snapshots:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- build \
  --input ./server/data/dev/gtfs.zip \
  --output ./server/data/dev/normalized \
  --bike-stations-taipei ./server/data/dev/tdx/taipei-bike-stations.json \
  --bike-stations-new-taipei ./server/data/dev/tdx/new-taipei-bike-stations.json
```

The build maps TDX station IDs to city-scoped Via IDs and uses Valhalla to
precompute bounded directional walking links from transit boarding points.
Live bikes, docks, and service status still come from request-time TDX
availability. The two JSON files must be supplied together.

## Inspect

```bash
cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect stop \
  --artifact ./server/data/dev/normalized/transit-network.json \
  --query "台北車站"

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect pattern \
  --artifact ./server/data/dev/normalized/transit-network.json \
  --query "307"

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect nearby-stops \
  --artifact ./server/data/dev/normalized/transit-network.json \
  --lat 25.0339 --lon 121.5645 --limit 20

cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect stats \
  --artifact ./server/data/dev/normalized/transit-network.json
```

## Route debugging

`via-route` uses the production transit planning flow through static Top-K
corridors, a request-local timing snapshot, fixed corridor evaluation,
reconstruction, and ranking. The offline CLI does not configure BikeShare;
production HTTP assembly uses the BikeShare artifact and TDX availability
source. For a deterministic offline route, use the provider-neutral fixture:

```bash
cargo run --manifest-path server/Cargo.toml --bin via-route -- \
  --network ./server/fixtures/routing/network.json \
  --from-lat 25.1000 --from-lon 121.5000 \
  --to-lat 25.1030 --to-lon 121.5030 \
  --depart-at 2026-08-31T10:00:00+08:00 \
  --modes bus,metro,tra,thsr \
  --service-conditions fixture \
  --trace
```

For live realtime, build the local GTFS artifact and use the live service
condition source. The live route needs `TDX_CLIENT_ID` and
`TDX_CLIENT_SECRET` in the environment or `.env` file. TDX is not used to build
static transit stops, routes, patterns, trips, calendars, frequencies,
transfers, or timing metadata.

```bash
cargo run --manifest-path server/Cargo.toml --bin via-route -- \
  --network ./server/data/dev/normalized/transit-network.json \
  --from-lat 25.0478 --from-lon 121.5170 \
  --to-lat 25.0339 --to-lon 121.5645 \
  --depart-now \
  --modes bus,metro,tra,thsr \
  --service-conditions live \
  --trace
```

The CLI also accepts `--depart-at <RFC3339>`, `--arrive-by <RFC3339>`, and
`--json`. `--modes` accepts comma-separated `bus`, `metro`, `tra`, and `thsr`.
Walking remains an implicit connector. BikeShare can realize a retained
surface transfer when production has station metadata and fresh availability;
it does not create a direct BikeShare-only route.

`TransferShortcutIndex` is projected from validated directional
`TransitNetwork.transfer_edges`. During `via-data build`, Valhalla generates
walking transfers between nearby boarding points and stores them in the
network. GTFS `transfers.txt` is ignored.
