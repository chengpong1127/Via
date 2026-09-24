# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using an immutable
GTFS-built transit network, request-local TDX realtime, Valhalla street
routing, and provider-neutral API models.

## Request flow

```text
PlanningRequest
  → RoutingService
  ├─ direct walking
  └─ mode-aware transit access / egress
       → TransitPlanner
       → static Top-K corridor search (forward, one global mode budget)
       → TimingSnapshotLoader (retained positions only; all provider I/O)
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

The static search starts at all origin access points and uses one global K
across every selected transit mode. It ranks structurally deduplicated corridors
with access/egress walking, GTFS and preprocessed walking transfer edges,
exact static travel times where available, derived Bus segment
costs where available, and documented geometry/speed estimates otherwise.
Derived Bus costs are not timetable data. Phase 2 may use them downstream of a
TDX boarding prediction when a Bus service has no complete GTFS trip. BikeShare
is not part of this phase.
Geometry fallback assumes zero wait and optimistic
speeds of 60 km/h for Bus, 100 km/h for Metro, 160 km/h for Rail, and 350 km/h
for High Speed Rail. The retained corridor union defines the small set of
boarding positions, services, and Bus stops used by Phase 2.

Phase 1 groups services by exact mode and full stop pattern, then reuses
zero-wait ride profiles and minimum adjacent costs across equivalent services.
Retention seeds the best corridor for each distinct mode-transition sequence,
then fills the same global K budget in duration order. A redundant single-mode
family therefore cannot remove every useful multimodal alternative. This is
structural diversity, not a per-mode quota; the search safety limit is unchanged.
Services exposing the same mode and board/alight segment as a retained corridor
remain in the Phase 2 candidate union, including overlapping pattern variants.

`TransitPlanner` creates timing queries only for that retained union and awaits
`TimingSnapshotLoader` before calling temporal search. `DepartureDispatcher`
groups these queries by mode;
each mode source owns provider-native batching and static fallback. Bus queries
are collapsed to unique `StopUID` values per realtime scope and chunked at the
provider boundary. Returned rows are normalized with direction-sensitive TDX
service IDs and filtered locally. The first rate limit for a Bus scope prevents
additional chunks for that scope; unresolved values fall back to static timing.

Temporal search is a synchronous forward round search over `TransitNetwork`,
`TransitTopologyIndex`, `TransferShortcutIndex`, `CandidateCorridor`, and
`TimingSnapshot`. It checks corridor membership and the remaining-leg budget
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

`via-data` reads the local GTFS zip and normalizes stops, routes, trips,
patterns, calendars, frequencies, transfers, and static timing metadata. It
uses Valhalla walking matrices to derive directional transfers between nearby
boarding points and stores them in `TransitNetwork.transfer_edges`. The build
retains at most 12 outgoing transfers per boarding point. It does not download
or refresh GTFS. TDX APIs supply realtime timing, service
conditions, and BikeShare availability only. GTFS IDs are normalized to Via's
existing TDX-derived IDs when their format maps directly; the build reports
unmapped IDs. The server has no database or identity reconciliation layer.

GTFS does not contain the BikeShare station catalog, so `via-data` writes an
empty BikeShare artifact. BikeShare alternatives require a station network
from a future static source.

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
