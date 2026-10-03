# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using an immutable
GTFS-built transit network, request-local TDX realtime, Valhalla street
routing, and provider-neutral API models.

## Request flow

```text
build_runtime
  → ViaRuntime
       ├─ routing: RoutingService
       └─ health: Health
  ↓
PlanningRequest
  → RoutingService.plan()
       ├─ direct alternatives
       │    └─ direct walking and a direct BikeShare seed
       ├─ transit alternatives
       │    ├─ TransitPlanner discovers access and egress points
       │    ├─ Phase 1: bounded static Top-K structural corridors
       │    ├─ one request-local TimingSnapshot
       │    ├─ optional BikeAvailabilitySnapshot
       │    └─ Phase 2: forward fixed-corridor evaluation
       ├─ merge stage outputs and failures
       ├─ JourneyReconstructor assembles candidates
       └─ validate failures → rank → PlanningOutcome
```

`ViaRuntime` is the shared concrete application assembly used by both the HTTP
server and `via-route`. `RoutingService` owns the complete planning use
case and combines explicit direct and transit stage outputs. Its request-scoped
`JourneyPlanningContext` and `JourneyPlanningLimits` live in `routing/model.rs`.

`TransitPlanner` owns transit access and egress discovery as well as the two
planning phases. Its topology and transfer indexes stay private. Phase 2
evaluates each retained Phase 1 corridor in order. It cannot add an anchor or
combine steps from different corridors. `JourneyReconstructor` owns place
lookups, street hydration, BikeShare station lookup, and candidate assembly;
it depends on the street router, transit runtime, and BikeShare network rather
than on `RoutingService`.

## Static corridor discovery

`TransitNetwork` stores Via Routes, ordered Patterns, Trips, Service Calendars,
Frequencies, and directional Transfers. `TransitTopologyIndex` stores
`PatternPosition(pattern_id, stop_index)` occurrences by boarding point, so
loop routes remain unambiguous. The static search uses one global K across all
selected transit modes. It ranks structurally distinct corridors using access
and egress walking, Via-preprocessed transfer walking, exact static segment
times where available, derived Bus segment costs, and documented optimistic
geometry estimates otherwise. Bike availability never influences Phase 1.

Sparse Bus stop times do not become fabricated `ScheduledTrip` values. When
terminal runtime is sufficient, preprocessing derives per-segment Bus costs
from stop or shape distances; otherwise it uses geometric estimates. These
costs rank Phase 1 candidates and are not timetable data.

Retained corridors contain equivalent transit legs, timing positions, and
selected Bus stops. Equivalent legs share mode and boarding/alighting points;
Phase 2 can choose any matching Pattern and departure while preserving the
retained structural steps.

`TransitRuntime::network()` is the explicit raw immutable network access used
by static search, timing providers, and data inspection. Health reads revisions
and validates through focused `TransitRuntime` operations. Data builds return
one `BuildOutput`; `BuildDiagnostics` groups GTFS, transfer, and BikeShare
diagnostics.

## Request-local evaluation

The planner loads one timing snapshot for the retained timing positions and,
when selected, one BikeShare availability snapshot. Bus realtime is fetched by
unique stop and scope, then filtered locally. Other modes keep their
provider-native batching and static fallback behavior.

Phase 2 evaluates normal transit and walking successors before screening a
BikeShare movement with a cheap optimistic lower bound. Competitive movements
are routed lazily through the street provider. The evaluator advances only
along retained corridor anchors. An active rental can extend from its original
pickup anchor to the next corridor anchor, so intermediate anchors need no
station and the rental remains continuous. Each movement requires a fresh,
serviceable pickup with a bike and a return station with a dock, and includes
walking access, one bicycle route, and walking from the return station. At
each endpoint it considers no more than three pickup and three return
stations. Transit departures after a movement are selected from the same
timing snapshot using the new arrival time. State expansion and retained
frontiers are bounded by the query's expansion and candidate limits.

The direct walking candidate uses the same journey path representation and may
have a separate direct origin-to-destination BikeShare alternative. It
considers three nearby serviceable stations at each endpoint and caps station
access walks at 15 minutes, bicycle legs at 60 minutes, and bicycle distance
at 20 kilometres. BikeShare never adds arbitrary transit stops, patterns, or
connections. Availability is observed at request time; future DepartAt and
ArriveBy inventory is not forecast.

Static transfer edges are directional facts in `TransitNetwork.transfer_edges`.
`TransferShortcutIndex` provides incoming and outgoing lookups; there is no
async transfer resolver.

## Static and live data

`via-data` reads the supplied local GTFS archive and does not download or
refresh it. GTFS is the only static transit source. TDX IDs are normalized to
existing Via IDs when their formats map directly; unmapped IDs are reported.
The server has no database or identity-reconciliation layer.

GTFS does not contain a YouBike station catalog. To build the static station
metadata, `via-data build` can take local TDX Taipei and New Taipei
`Bike/Station/City` JSON snapshots. It maps their station IDs to the same
city-scoped Via IDs used by availability, then precomputes bounded directional
walking links with Valhalla. Live bike and dock counts, service state, and
freshness come from TDX request-time availability. Without station snapshots,
the generated BikeShare artifact remains empty.

Keep the dependency direction `domain ← routing ← app/api`. Providers
implement domain-facing interfaces. iOS remains feature-oriented
(`App`, `Components`, `Domain`, `Features`, `Resources`, `Services`), with
production planning through `JourneyPlanner → HTTPJourneyPlanner → APIClient`.
The server parses runtime environment settings at its executable entrypoints
and passes typed configuration into provider and data-fetch clients. Production
requires HTTPS for configured upstream URLs; `via-data build` keeps its
separate 15-second Valhalla request timeout.

## Essential commands

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path server/Cargo.toml --all-targets --all-features
```

See [`docs/dev-tooling.md`](docs/dev-tooling.md) for data and route-debug
commands.
