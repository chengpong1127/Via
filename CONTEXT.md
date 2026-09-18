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
  → reconstruction
  → ranking
  → JourneyOption
```

`RoutingAlgorithm` is the algorithm-neutral boundary. The current implementation
is `TripBasedRouter`, which owns its Trip-Based index and search state. The
service does not depend on a graph or a second planner abstraction.

## Static data

`via-data` fetches TDX payloads, normalizes them into the provider-neutral
`TransitNetwork`, enriches directional walking transfers through the
`StreetRouter`, validates the network, and writes
`transit-network.json` plus `manifest.json`. The runtime loads one immutable
artifact revision; it has no database or provider identity-reconciliation layer.

TDX-derived ID constructors are shared by static normalization and realtime
normalization. Keep `BoardingPointId`, `TransitServiceId`, `ScheduledTripId`,
and `EffectiveTripId` distinct.

## Routing and transfers

`TripBasedRouter` owns transit search, including same-stop interchange, and
consumes `DepartureResolver` plus a batch `TransferResolver` for movement between
distinct `BoardingPoint`s. The only current resolver uses an in-memory
`TransferShortcutIndex` derived from `TransitNetwork.transfer_edges`. Future
dynamic mobility can be implemented behind `TransferResolver` without changing
Trip-Based search.
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
request-local effective-trip materialization, and static fallback. The generic
realtime provider/cache remains only for request-scoped service-condition
status collection; it is not part of departure timing. Metro, TRA, and THSR
remain static-only until their own sources implement a direct TDX integration.

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
  directly, while `RealtimeProvider` is reserved for service-condition/status
  collection.
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
