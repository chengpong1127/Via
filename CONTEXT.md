# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using immutable TDX
transit artifacts, request-local TDX realtime, Valhalla street routing, and
provider-neutral API models.

## Request flow

```text
PlanningRequest
  → RoutingService
  ├─ direct walking
  └─ mode-aware transit access / egress
       → TransitPlanner
       → DestinationReachability (backward, topology only)
       → TimingSnapshotLoader (all departure provider I/O)
       → temporal search (forward, local and synchronous)
       → optional BikeAvailabilitySnapshot
       → adjacent transit/walking segment alternatives
  → reconstruction
  → ranking
  → JourneyOption
```

`TransitPlanner` is concrete; there is no routing-algorithm trait, legacy
Trip-Based implementation, route graph, or wrapper planning service.

## Transit search

`TransitTopologyIndex` stores only `(service_id, stop_index)` occurrences by
boarding point. Using a position index keeps loop routes unambiguous without
duplicating service patterns.

`DestinationReachability` starts at destination egress stops and traverses
service patterns backward for at most `max_transfers + 1` transit legs. Between
rides it considers the same stop or one directional static walking transfer
through `TransferShortcutIndex::transfers_to`. It filters only by topology,
selected transit modes, transfer direction, and leg budget. It does not import
or inspect departure times, timetables, realtime, or effective trips.

The reachability result defines the timing surface. `TransitPlanner` creates one
request-local set of timing queries and awaits `TimingSnapshotLoader` before
calling temporal search. `DepartureDispatcher` groups these queries by mode;
each mode source owns provider-native batching and static fallback. Bus queries
are collapsed to unique `StopUID` values per realtime scope and chunked at the
provider boundary. Returned rows are normalized with direction-sensitive TDX
service IDs and filtered locally. The first rate limit for a Bus scope prevents
additional chunks for that scope; unresolved values fall back to static timing.

Temporal search is a synchronous forward round search over `TransitNetwork`,
`TransitTopologyIndex`, `TransferShortcutIndex`, `DestinationReachability`, and
`TimingSnapshot`. It checks structural reachability and the remaining-leg budget
before every snapshot lookup. Its dominance rule keeps the earliest ready time
for each `(boarding point, legs used)`. Downstream rides can yield destination
egress candidates, same-stop interchange, or one outgoing static walking
transfer. No TDX client, HTTP type, provider adapter, or await point is reachable
from temporal search.

## Timing

`EffectiveTrip`, `EffectiveStopTime`, `TimingBasis`, `TimingReliability`, and
normalized realtime evidence remain provider-neutral domain values. Bus, Metro,
TRA, and THSR sources retain their established timing policies: Bus/Metro/TRA
prefetch realtime where available and degrade to their static behavior; THSR is
static-only. Timing provenance does not imply reliability.

No shared realtime TTL cache, Redis layer, singleflight system, or distributed
cache exists. `TimingSnapshot` is request-local and is the future caching seam.

## Transfers and multimodal planning

Static transit transfers are directional facts in
`TransitNetwork.transfer_edges`. Runtime builds one `TransferShortcutIndex` with
both incoming and outgoing lookup maps. There is no async transfer-provider
abstraction.

Direct walking remains independent of transit reachability and does not receive
BikeShare variants. Phase 2 projects each temporal transit candidate to a
`JourneySkeleton`, loads BikeShare availability once when selected, and may
replace only an adjacent transit movement or static walking transfer between
boarding-point anchors. It keeps the baseline and bounded alternatives, then
reconstructs and ranks the final variants. BikeShare does not create new transit
connectivity, replace access/egress, or create a direct BikeShare-only route.

## Static data and boundaries

`via-data` fetches and normalizes TDX static transit and BikeShare payloads,
enriches static walking relationships through `StreetRouter`, validates the
artifacts, and writes versioned JSON. Static and realtime normalization share
the pure TDX-derived ID constructors. The server has no database or identity
reconciliation layer.

Keep provider adapters out of domain and routing. Providers implement the
domain-facing `TimingSnapshotLoader`, `TransitDepartureSource`, and service
condition boundaries. `TdxClient` remains transport-agnostic and owns only
OAuth, bounded HTTP, generic decoding, and shared ID constructors.

## Essential commands

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features
cargo test --manifest-path server/Cargo.toml --all
```

See [`docs/dev-tooling.md`](docs/dev-tooling.md) for data and route-debug
commands.
