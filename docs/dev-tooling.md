# Via development data and routing tools

Static transit data is built only from the local GTFS archive:

```text
GTFS zip → via-data build → validated TransitNetwork → Phase 1 static Top-K
TDX realtime APIs → TimingSnapshot → Phase 2 temporal search
```

`via-data` does not download or refresh the archive. It reads the supplied zip
in place and writes the normalized network, an empty BikeShare artifact, and a
manifest. The current development archive is `server/data/dev/gtfs.zip`.

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
present. It ignores `transfers.txt`. Bus
stop-time rows with incomplete intermediate times are not turned into
`TransitTrip` records. When terminal anchors provide a full runtime, the
builder distributes that runtime across adjacent stops using GTFS shape
distance, then geographic distance as a fallback. The per-segment median is a
persisted Pattern static profile and can time downstream segments after a realtime Bus
boarding prediction; it is not an exact timetable. Patterns without usable
static timing receive an optimistic geometric profile during preprocessing.

The build output is:

```text
transit-network.json
provider-bindings.json
bikeshare-network.json
manifest.json
```

Revisions are hashes of canonical normalized content. The manifest includes
the local archive hash and counts. GTFS has no BikeShare station catalog, so
the generated BikeShare artifact is empty; BikeShare variants are unavailable
until a static station source is added outside this transit GTFS import.

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

`via-route` uses the production
`RoutingService → TransitPlanner → static Top-K corridors → TimingSnapshot →
temporal search → reconstruct → rank` path. For a deterministic offline route,
use the provider-neutral fixture network:

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

For live realtime, build the local GTFS artifact and switch the service
condition source to `live`. Bus ETA, service conditions, and BikeShare
availability are realtime provider inputs; TDX is not used to build static
transit stops, routes, patterns, trips, calendars, frequencies, transfers, or timing metadata.
Only the live route requires `TDX_CLIENT_ID` and `TDX_CLIENT_SECRET` in the
environment or `.env` file.

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
The offline CLI rejects `bike_share` explicitly. BikeShare may replace an
existing adjacent transit stop movement when a station network and realtime
availability snapshot are configured; it does not create transit connectivity
or origin/destination access alternatives.

`TransferShortcutIndex` is projected directly from validated
`TransitNetwork.transfer_edges` at server or CLI startup. Public-transit
transfer reconstruction does not call Valhalla. During `via-data build`, Via
generates directional walking transfers from boarding-point coordinates and
Valhalla, then stores them in `TransitNetwork.transfer_edges`. GTFS
`transfers.txt` is ignored.
