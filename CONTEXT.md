# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using an immutable
GTFS-built transit network, request-local TDX realtime, Valhalla street
routing, and provider-neutral API models.

## Request flow

```text
PlanningRequest
  → RoutingService.plan()
       ├─ discover transit access / egress when transit is selected
       ├─ MultimodalPlanner.prepare() → PlanningSession.evaluate_primary()
       │    ├─ Phase 1: bounded static Top-K transit corridors
       │    ├─ load request-local snapshots for retained transit positions
       │    ├─ convert retained corridors to EvaluationCorridors
       │    └─ Phase 2: fixed-corridor movement evaluation
       ├─ PlanningSession.evaluate_fallback() via EvaluationCorridor::surface_only() when needed
       ├─ JourneyReconstructor assembles candidates
       └─ validate failures → rank → PlanningOutcome
```

`ViaRuntime` is the shared concrete application assembly used by both the HTTP
server and `via-route`. `RoutingService` owns the complete planning use case.
Its request-scoped `JourneyPlanningContext` and `JourneyPlanningLimits` live in
`routing/model.rs`.

`RoutingService` discovers access and egress for selected transit modes, then
calls `MultimodalPlanner.prepare()` and evaluates the request-local
`PlanningSession`. Primary evaluation uses `evaluate_primary()`; after all
primary candidates fail reconstruction, the session can evaluate its one-shot
`evaluate_fallback()` corridor built with `EvaluationCorridor::surface_only()`.
The planner owns static corridor discovery, request snapshots, and Phase 2. Its
topology and transfer indexes stay private. A Phase-2 `EvaluationCorridor` has
explicit Origin,
BoardingPoint, and Destination anchors. Its edge baseline can hold bounded
access or egress alternatives, transit, retained walking, or no movement.
Phase 2 evaluates each corridor independently and cannot add anchors or
combine steps from different corridors. `RealizedJourney` is the sole realized
route value: it owns ordered anchors and movements, validates path continuity
and timing, and supplies canonical movement identity for deduplication.
`JourneyReconstructor` owns place lookups, remaining street geometry hydration,
BikeShare station lookup, and candidate assembly; it depends on the street
router, transit runtime, and BikeShare network rather than on `RoutingService`.

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

Walking is always implicit and is not a `selected_modes` value. An empty mode
list skips transit discovery and Phase 1, then evaluates a surface-only
Origin-to-Destination corridor as walking. Selecting only BikeShare also skips
transit search and timing requests while loading one BikeShare availability
snapshot. If transit corridors evaluate to no candidate, the planner evaluates
the same surface-only corridor as a fallback. It does not append a direct route
to successful transit results. The session retains its snapshots, movement
caches, and expansion accounting through reconstruction, so failed reconstruction
can call its one-shot fallback without additional snapshot loads. The fallback flag is internal session state.

For each edge, `FixedCorridorEvaluator` evaluates its baseline and asks the
enabled `SurfaceMovementPlanner`s about alternatives. Walking and BikeShare
first use cheap optimistic duration bounds; competitive routes are realized
lazily through the street provider and cached within the request. Walking
alternatives are retained as already-routed path movements, avoiding a second
route lookup during reconstruction. BikeShare considers up to three pickup
and three return stations per endpoint. A continuous rental can extend from
its original anchor to a later corridor anchor, so intermediate anchors need
no station. Separate rentals obey the 15-minute reuse rule. BikeShare requires
a fresh, serviceable pickup with a bike and a return station with a dock; it
includes station access, one bicycle route, and station exit walking. Exact
bicycle legs are capped at 60 minutes and 20 kilometres. Availability is
observed at request time; future DepartAt and ArriveBy inventory is not
forecast.

Transit departures after a movement are selected from the same timing
snapshot using the new arrival time. State expansion and retained frontiers
are bounded by the query's expansion and candidate limits. Walking and
BikeShare cannot add transit stops, patterns, or connections.

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
