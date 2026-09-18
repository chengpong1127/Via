# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using TDX transit data,
Valhalla street routing, and TDX realtime status. The server returns provider-
neutral `JourneyOption` values for walking, transit, and BikeShare journeys.

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
`TransitNetwork`, enriches directional walking transfers and transit↔BikeShare
walking connectors through the `StreetRouter`, validates the network, and writes
`transit-network.json` plus `manifest.json`. The runtime loads one immutable
artifact revision; it has no database or provider identity-reconciliation layer.

TDX-derived ID constructors are shared by static normalization and realtime
normalization. Keep `BoardingPointId`, `BikeShareStationId`, `TransitServiceId`,
`ScheduledTripId`, and `EffectiveTripId` distinct.

## Routing and transfers

`TripBasedRouter` consumes `DepartureResolver`, `EffectiveTrip`, and an in-memory
`TransferShortcutIndex` derived from `TransitNetwork.transfer_edges`.
`DepartureResolver` is realtime-first with static fallback. A resolved
`EffectiveTrip` carries its timing basis and optional evidence; scheduled and
effective trip identity must not be conflated.

Static transit transfers are directional walking facts preprocessed during the
data build. Runtime reconstruction uses their duration and synthetic walking
geometry; it does not call Valhalla for those transfer legs.

## BikeShare

Static `TransitNetwork.bike_share_connectors` contain directional walking facts
between transit boarding points and BikeShare stations. `StreetBikeShareOperator`
handles query-time station candidate selection, realtime availability, bounded
bicycle matrices, and destination walking.

The operator can provide:

- direct origin→BikeShare→destination journeys;
- origin→BikeShare→transit access;
- transit→BikeShare→transit transfers; and
- transit→BikeShare→destination egress.

Availability is checked for the pickup and return stations at query time. A
`MobilityPath` contains walking and BikeShare legs; `TransferShortcut` is only
for static transit-to-transit walking.

## Invariants

- Keep `StreetRouter`, `RoutingAlgorithm`, and `DepartureResolver` as the
  provider/algorithm-neutral seams.
- Keep Trip-Based search, static transfer preprocessing, and the current
  BikeShare operator as separate current responsibilities.
- Keep provider adapters out of the domain and routing modules.
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
