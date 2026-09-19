# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using TDX transit data,
Valhalla street routing, and TDX realtime status. The server returns provider-
neutral `JourneyOption` values for walking and transit journeys.

## Request flow

```text
PlanningRequest
  → RoutingService
  → street access / egress
  → RoutingAlgorithm
  → TripBasedRouter
  → JourneyCandidate
  → JourneySkeleton
  → JourneyAugmenter (baseline only)
  → JourneyVariant
  → reconstruction
  → ranking
  → JourneyOption
```

`RoutingAlgorithm` is the algorithm-neutral boundary. The current implementation
is `TripBasedRouter`, which owns its Trip-Based index and search state. The
service does not depend on a graph or a second planner abstraction.

## Static data

`via-data` fetches TDX payloads, normalizes them into the provider-neutral
`TransitNetwork`, normalizes static TDX Bike Station payloads into a separate
provider-neutral `BikeShareNetwork`, enriches directional transit walking
transfers and BoardingPoint ↔ bike-station walking access through the
`StreetRouter`, validates both networks, and writes `transit-network.json`,
`bikeshare-network.json`, and `manifest.json`. Bike access is static build-time
data only: realtime availability, bicycle routing, BikeShortcut, and journey
augmentation do not consume it yet. The runtime loads one validated immutable
transit artifact and one validated immutable bike-share artifact; there is no
database or provider identity-reconciliation layer.

The TDX static bike source is `/v2/Bike/Station/City/{City}` for the selected
`DataScope` cities. Bike station IDs are opaque domain IDs constructed as
`tdx:{City}:{StationUID}` so Taipei and New Taipei remain collision-safe even
if a provider station ID is only locally unique. Each retained bike access link
can carry independent `walk_to_station` and `walk_from_station` metrics.

TDX-derived ID constructors are shared by static normalization and realtime
normalization. Keep `BoardingPointId`, `TransitServiceId`, `ScheduledTripId`,
and `EffectiveTripId` distinct.

## Routing and transfers

`TripBasedRouter` owns transit search, including same-stop interchange, and
consumes `DepartureResolver` plus a batch `TransferResolver` for movement between
distinct `BoardingPoint`s. The only current resolver uses an in-memory
`TransferShortcutIndex` derived from `TransitNetwork.transfer_edges`. Future
dynamic mobility belongs after transit `JourneyCandidate` creation, in journey
augmentation, and must not be implemented as `TransferResolver` behavior.

Journey augmentation is a separate seam after `JourneyCandidate` creation and
before reconstruction. `JourneySkeleton` projects only origin, boarding-point,
and destination anchors plus indexed baseline movement. The current
`BaselineJourneyAugmenter` returns one `JourneyVariant` containing the original
path, so it does not change route search, reconstruction, ranking, or the public
`JourneyOption` contract. Trip-Based search remains unaware of augmentation;
future mobility shortcuts must not be added as `TransferResolver` behavior.
`DepartureResolver` is the algorithm-facing timing boundary. Its current
implementation is a `DepartureDispatcher` that groups queries by
`TransitMode` and delegates to mode-owned sources. Each source owns its static
timing, optional realtime timing, per-query fallback policy, and timing
reliability policy. A resolved `EffectiveTrip` carries both timing provenance
(`TimingBasis`) and an explicit `TimingReliability`; provenance does not imply
trust. Static timing may be reliable: THSR is the canonical static-only
example, while bus static fallback and Metro frequency/timetable timing remain
fallback reliability under their source policies. Scheduled and effective trip
identity must not be conflated.

Bus realtime timing is owned by `BusDepartureSource`, which calls the shared
transport-only `TdxClient` directly. The source owns the bus endpoint path,
scoped OData query, private TDX DTO parsing, freshness checks, batching,
request-local effective-trip materialization, and static fallback. TRA timing
is likewise owned by `TraDepartureSource`: it batches exact `tra:{TrainNo}`
queries against TDX's TRA `v3/Rail/TRA/TrainLiveBoard`, applies a fresh delay to the
static scheduled trip shape, and falls back to the schedule on missing, stale,
malformed, or failed realtime. The `ServiceConditionProvider` and
`CachedServiceConditionProvider` remain only for request-scoped
service-condition/status collection; they are not part of departure timing.
Metro departure timing is also owned by `MetroDepartureSource`, which calls
TDX's [`v2/Rail/Metro/LiveBoard/TRTC`](https://tdx.transportdata.tw/api-service/swagger/basic/268fc230-2e04-471b-a728-a726167c1cfc)
directly. It matches fresh station-level LiveBoard predictions by line,
boarding station, and destination terminal (the directional service identity),
then uses the existing Metro inter-station travel-time shape to materialize the
rest of the effective trip. Missing, stale, malformed, unmatched, or failed
Metro realtime falls back to the existing station timetable/frequency source.
THSR remains static-only.

The production boundaries are:

```text
TripBasedRouter
  → DepartureResolver
  → DepartureDispatcher
  → BusDepartureSource ─────→ TdxClient
    MetroDepartureSource ────→ TdxClient
    TraDepartureSource ─────→ TdxClient
    ThsrDepartureSource

RoutingService
  → ServiceConditionProvider
  → TdxServiceConditionProvider
  → TdxClient
```

`TdxClient` lives under `server/src/tdx/` and owns only OAuth, HTTP limits,
generic JSON decoding, and small shared TDX ID constructors. Service-condition
DTO parsing belongs to `providers/service_conditions/tdx.rs`; Bus DTO parsing
and timing semantics belong to `providers/timing/bus_realtime.rs`.

Static transit transfers are directional walking facts preprocessed during the
data build. Runtime reconstruction uses their duration and synthetic walking
geometry; it does not call Valhalla for those transfer legs.

## Invariants

- Keep `StreetRouter`, `RoutingAlgorithm`, and `DepartureResolver` as the
  provider/algorithm-neutral seams.
- Keep `TripBasedRouter` unaware of transport-specific timing sources. Bus,
  Metro, TRA, and THSR timing policies belong under
  `server/src/providers/timing/`.
- Keep `TimingBasis` for provenance and diagnostics; use explicit timing
  reliability for ranking. Do not classify every static timetable as
  `JourneyClass::Feasible`.
- Keep Trip-Based search and static transfer preprocessing as separate current
  responsibilities.
- Keep provider adapters out of the domain and routing modules.
- Keep departure timing mode-owned: `BusDepartureSource` may use `TdxClient`
  directly, while `ServiceConditionProvider` is reserved for
  service-condition/status collection.
- Keep `TdxClient` transport-agnostic. It owns OAuth, base URL, HTTP limits,
  and generic JSON decoding, not transport-specific DTOs or timing policy.
- Do not reintroduce `planner`, `normalization`, `routing/engine`, `RouteGraph`,
  or a `JourneyPlanningService` wrapper.
- Navigation is not part of the current implementation.

## Essential commands

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features
cargo test --manifest-path server/Cargo.toml --all

cargo run --manifest-path server/Cargo.toml --bin via-data -- fetch \
  --scope taipei-new-taipei --date YYYY-MM-DD --output ./data/dev/taipei
cargo run --manifest-path server/Cargo.toml --bin via-data -- build \
  --input ./data/dev/taipei/raw/YYYY-MM-DD --output ./data/dev/taipei
cargo run --manifest-path server/Cargo.toml --bin via-data -- validate \
  --artifact ./data/dev/taipei/transit-network.json
cargo run --manifest-path server/Cargo.toml --bin via-data -- inspect stats \
  --artifact ./data/dev/taipei/transit-network.json
```

See [`docs/dev-tooling.md`](docs/dev-tooling.md) for the current local data and
route-debug workflow.
